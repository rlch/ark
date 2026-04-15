//! T-081: Codex findings watcher (cavekit-orchestrator-cavekit R7).
//!
//! Watches `{cwd}/context/impl/impl-review-findings.md` — the canonical
//! output location of the Codex adversarial reviewer driven by
//! `codex-review.sh` — and translates new findings rows into
//! [`AgentEvent::ReviewComment`] events on the shared bus.
//!
//! ## File format
//!
//! The reviewer produces a markdown table whose body rows look like:
//!
//! ```text
//! | F-018: Center and Align now always emit width: 100% ... (source: codex) | P1 | path/to/file.rs:L30-35 | NEW | — |
//! ```
//!
//! The columns we care about are:
//! 1. **Finding** — `F-NNN: description (source: codex)`. The leading
//!    `F-NNN` is the stable finding id we dedupe against. The body we
//!    emit is the description portion, trimmed of the trailing
//!    `(source: codex)` attribution if present.
//! 2. **Severity** — case-insensitive `P0 | P1 | P2 | P3`. Tolerates the
//!    `P1 (high)` style the reviewer sometimes emits.
//! 3. **File** — `path[:LINE]` (with optional `L`-prefixed line, e.g.
//!    `foo.rs:L42` or `foo.rs:42-50`; we take the first integer as the
//!    anchor line).
//!
//! ## Emit rules
//!
//! - `enabled = false` → return `Ok(())` immediately.
//! - File absent → watch the parent dir, wait for the file to appear.
//! - Debounced at 250ms (bursty writes during the reviewer's final
//!   table render).
//! - Each **distinct** `F-NNN` fires exactly one `ReviewComment` across
//!   the lifetime of the watcher. Re-writes of the same table do not
//!   re-emit.
//! - Rows whose description matches the `NO_FINDINGS` sentinel heuristic
//!   (empty body + severity column contents like `P1 (high)` or literal
//!   `NO_FINDINGS`) are skipped — those are meta-artefacts of the
//!   reviewer prompt leaking through, not actual findings.
//! - Reviewer id is a synthetic [`AgentId::new("codex", "reviewer")`].
//!   The kit calls this a "subagent id for cross-ref"; v1 takes the
//!   simplest stable form.
//!
//! ## Rollup API
//!
//! [`parse_findings`] + [`rollup`] are exposed as pure functions for
//! orchestrator-side consumers that want severity counts without
//! subscribing to the event bus (e.g. rendering a status line). The
//! watcher itself only emits events; rollup is an orthogonal surface.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, CancellationToken, EventSink, Severity};
use notify::{RecursiveMode, Watcher};

/// Debounce window — reviewer sometimes writes the table in a burst
/// (header, separator, rows) across a few hundred milliseconds.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Sentinel text fragment the reviewer prompt leaks into empty-findings
/// meta rows. Any row whose body contains this string (case sensitive;
/// the reviewer emits it verbatim) is skipped.
const NO_FINDINGS_SENTINEL: &str = "NO_FINDINGS";

/// A single parsed finding — the public data shape for [`parse_findings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Finding id (e.g. `F-017`). Used for dedup across re-parses.
    pub id: String,
    /// Severity (`P0`..`P3`).
    pub severity: Severity,
    /// File path from the third column.
    pub path: PathBuf,
    /// Optional source line anchor (first integer after `:` or `:L`).
    pub line: Option<u32>,
    /// Description text, with `(source: ...)` attribution stripped.
    pub body: String,
}

/// Severity-bucketed counts produced by [`rollup`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindingsRollup {
    pub counts_by_severity: BTreeMap<Severity, u32>,
    pub total: u32,
}

/// Public entry point — see module docs.
pub async fn watch_codex_findings(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    let dir = cwd.join("context").join("impl");
    let file = dir.join("impl-review-findings.md");

    // Create the parent dir so we can watch it even before the reviewer
    // writes the findings file. Missing permissions → log + quiet exit.
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::debug!(
            path = %dir.display(),
            error = %e,
            "watch_codex_findings: could not create context/impl/ — exiting"
        );
        return Ok(());
    }

    let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = std_tx.send(res);
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;

    // Filename-only match: FSEvents on macOS canonicalizes TempDir paths
    // through `/private`, breaking parent-directory equality checks.
    let target_name = file
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let reader_handle = std::thread::spawn(move || {
        for res in std_rx {
            let Ok(event) = res else { continue };
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

    // Synthetic reviewer id. Stable for the lifetime of the watcher; the
    // kit only asks for a cross-ref handle (not a spawned agent).
    let reviewer = AgentId::new("codex", "reviewer");
    let mut emitted: HashSet<String> = HashSet::new();

    // Initial parse so any findings already on disk at start-up fire
    // ReviewComment events (dedup set is empty → all are new).
    reparse_and_emit(&file, &id, &reviewer, &tx, &mut emitted).await;

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
                reparse_and_emit(&file, &id, &reviewer, &tx, &mut emitted).await;
            }
        }
    }

    drop(watcher);
    let _ = reader_handle.join();
    Ok(())
}

/// Re-read the findings file, parse it, and emit `ReviewComment` for any
/// `F-ID` not seen before. Missing/unreadable file is a no-op.
async fn reparse_and_emit(
    file: &std::path::Path,
    id: &AgentId,
    reviewer: &AgentId,
    tx: &EventSink,
    emitted: &mut HashSet<String>,
) {
    let Ok(contents) = tokio::fs::read_to_string(file).await else {
        return;
    };
    for f in parse_findings(&contents) {
        if emitted.contains(&f.id) {
            continue;
        }
        emitted.insert(f.id.clone());
        let _ = tx.send(AgentEvent::ReviewComment {
            id: id.clone(),
            reviewer: reviewer.clone(),
            severity: f.severity,
            path: f.path,
            line: f.line,
            body: f.body,
        });
    }
}

/// Parse the full table body out of `contents`.
///
/// Returns findings in the order they appear in the file. Malformed rows
/// (too few columns, missing `F-ID`, un-parseable severity, NO_FINDINGS
/// sentinel) are silently skipped — this function never panics on input.
pub fn parse_findings(contents: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if !line.starts_with('|') || !line.ends_with('|') {
            continue;
        }
        // Strip outer pipes and split into cells.
        let inner = &line[1..line.len() - 1];
        let cells: Vec<&str> = inner.split('|').map(|s| s.trim()).collect();
        if cells.len() < 3 {
            continue;
        }

        // Column 0: `F-NNN: description (source: codex)` or header.
        let Some((fid, description)) = parse_finding_cell(cells[0]) else {
            continue;
        };

        // Column 1: severity. `P1`, `p2`, or `P1 (high)` → P1.
        let Some(severity) = parse_severity(cells[1]) else {
            continue;
        };

        // Column 2: path[:[L]line[-line2]] — skip rows whose path cell
        // itself looks like a leaked severity-legend (the NO_FINDINGS
        // meta rows from the reviewer have severity-legend strings here).
        let raw_path = cells[2];
        if looks_like_severity_legend(raw_path) {
            continue;
        }
        let (path, line_no) = parse_path_cell(raw_path);

        // Reject rows that look like the NO_FINDINGS sentinel meta row.
        // The reviewer emits these when it had nothing to say and the
        // prompt bled through into the table. Shape varies; we detect by
        // body/path containing the sentinel OR empty description.
        if description.is_empty()
            || description.contains(NO_FINDINGS_SENTINEL)
            || raw_path.contains(NO_FINDINGS_SENTINEL)
        {
            continue;
        }

        out.push(Finding {
            id: fid,
            severity,
            path,
            line: line_no,
            body: description,
        });
    }
    out
}

/// Bucket a slice of findings by severity + total count.
pub fn rollup(findings: &[Finding]) -> FindingsRollup {
    let mut counts_by_severity: BTreeMap<Severity, u32> = BTreeMap::new();
    for f in findings {
        *counts_by_severity.entry(f.severity.clone()).or_insert(0) += 1;
    }
    FindingsRollup {
        total: findings.len() as u32,
        counts_by_severity,
    }
}

/// Parse the first column, returning `(F-ID, body)` on success.
///
/// The cell is expected to be `F-NNN: body text (source: codex)`. We
/// extract the `F-NNN` token strictly, then return the remainder with a
/// trailing `(source: ...)` attribution stripped. Rows without the
/// `F-` prefix (e.g. table header `| Finding | ... |`) return `None`.
fn parse_finding_cell(cell: &str) -> Option<(String, String)> {
    let cell = cell.trim();
    if !cell.starts_with("F-") {
        return None;
    }
    let (fid_part, rest) = match cell.split_once(':') {
        Some(parts) => parts,
        // Tolerate `F-NNN` with no colon (malformed but present) — body
        // is then empty, which the NO_FINDINGS filter will drop.
        None => (cell, ""),
    };
    // Validate F-ID shape: `F-` followed by at least one digit.
    let fid = fid_part.trim();
    if !fid.starts_with("F-") || fid.len() < 3 || !fid[2..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let body = strip_source_attribution(rest.trim());
    Some((fid.to_string(), body))
}

/// Remove a trailing `(source: ...)` attribution from `s` (leaving any
/// earlier parenthetical prose intact). Returns a trimmed owned String.
fn strip_source_attribution(s: &str) -> String {
    // Find the **last** `(source:` opener; if it runs to a closing `)`
    // at end-of-string (ignoring trailing whitespace), drop it.
    let trimmed = s.trim_end();
    if let Some(open_idx) = trimmed.rfind("(source:") {
        if trimmed.ends_with(')') {
            return trimmed[..open_idx].trim_end().to_string();
        }
    }
    trimmed.to_string()
}

/// Parse a severity cell. Accepts `P0`..`P3`, case-insensitive, with
/// optional trailing qualifier like `(high)` or `- critical`.
fn parse_severity(cell: &str) -> Option<Severity> {
    let upper = cell.trim().to_ascii_uppercase();
    // Take the first whitespace-separated token; handles `P1 (high)`.
    let first = upper.split_whitespace().next()?;
    match first {
        "P0" => Some(Severity::P0),
        "P1" => Some(Severity::P1),
        "P2" => Some(Severity::P2),
        "P3" => Some(Severity::P3),
        _ => None,
    }
}

/// Parse a path cell into `(PathBuf, Option<line>)`. Accepts:
/// - `foo/bar.rs`
/// - `foo/bar.rs:42`
/// - `foo/bar.rs:L42`
/// - `foo/bar.rs:L42-50` (anchor = 42)
/// - `foo/bar.rs:42-50, 60-65` (anchor = 42)
fn parse_path_cell(cell: &str) -> (PathBuf, Option<u32>) {
    let cell = cell.trim();
    match cell.rsplit_once(':') {
        Some((path, line_part)) => {
            let line = extract_first_u32(line_part);
            // If the suffix produced no number at all, treat the whole
            // cell as a path (e.g. Windows `C:/foo` — unlikely here but
            // defensive).
            if line.is_none() {
                (PathBuf::from(cell), None)
            } else {
                (PathBuf::from(path), line)
            }
        }
        None => (PathBuf::from(cell), None),
    }
}

/// Heuristic: does this cell look like a leaked severity legend from the
/// reviewer's NO_FINDINGS meta row (contains ':L' followed by other
/// severity labels, or several `P#` tokens)?
fn looks_like_severity_legend(s: &str) -> bool {
    // NO_FINDINGS meta rows typically stuff the severity legend into the
    // file column: `P2 (medium):LP3 (low). No issues found = output
    // NO_FINDINGS alone.`
    let upper = s.to_ascii_uppercase();
    if upper.contains(NO_FINDINGS_SENTINEL) {
        return true;
    }
    // Two-or-more severity tokens in the file cell is always a legend.
    let p_tokens = upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| matches!(*t, "P0" | "P1" | "P2" | "P3"))
        .count();
    p_tokens >= 2
}

/// Extract the first contiguous run of ASCII digits from `s` and parse as
/// `u32`. Returns `None` if no digits are present or the number overflows.
fn extract_first_u32(s: &str) -> Option<u32> {
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !digits.is_empty() {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::channel;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_id() -> AgentId {
        AgentId::new("cavekit", "codex-findings")
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

    async fn wait_for_count<F: Fn(&AgentEvent) -> bool>(
        rx: &mut ark_types::EventReceiver,
        pred: F,
        want: usize,
        timeout: StdDuration,
    ) -> Vec<AgentEvent> {
        let start = std::time::Instant::now();
        let mut matched: Vec<AgentEvent> = Vec::new();
        while start.elapsed() < timeout && matched.len() < want {
            match tokio::time::timeout(StdDuration::from_millis(50), rx.recv()).await {
                Ok(Ok(ev)) => {
                    if pred(&ev) {
                        matched.push(ev);
                    }
                }
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        matched
    }

    // -------------------- watcher-level tests -----------------------------

    #[tokio::test]
    async fn disabled_returns_ok_immediately() {
        let tmp = TempDir::new().unwrap();
        let (tx, _rx) = channel(16);
        let cancel = CancellationToken::new();
        // If enabled=false doesn't early-exit, this hangs forever.
        watch_codex_findings(tmp.path().to_path_buf(), make_id(), tx, cancel, false)
            .await
            .expect("disabled ok");
    }

    #[tokio::test]
    async fn missing_file_emits_no_events() {
        let tmp = TempDir::new().unwrap();
        let (tx, mut rx) = channel(16);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_codex_findings(
            tmp.path().to_path_buf(),
            id,
            tx,
            cancel.clone(),
            true,
        ));

        // Give the watcher a beat to install, then cancel. No events.
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        cancel.cancel();
        let result = tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("join timeout")
            .expect("join");
        result.expect("watcher ok");
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn emits_review_comments_for_three_findings() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let file = impl_dir.join("impl-review-findings.md");
        std::fs::write(
            &file,
            "| Finding | Severity | File | Status | Task |\n\
             |---|---|---|---|---|\n\
             | F-001: Tick loop no-op (source: codex) | P0 | internal/tui/app.go:213-214 | NEW | T-043 |\n\
             | F-002: ActionOpen never handled (source: codex) | P1 (high) | internal/tui/app.go:109 | NEW | T-045 |\n\
             | F-003: Menu doesn't adapt | P2 | internal/tui/menu.go:44 | NEW | — |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_codex_findings(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let comments = wait_for_count(
            &mut rx,
            |e| matches!(e, AgentEvent::ReviewComment { .. }),
            3,
            StdDuration::from_secs(3),
        )
        .await;

        assert_eq!(
            comments.len(),
            3,
            "expected 3 ReviewComments, got {comments:?}"
        );

        // Verify F-001 (P0, line 213).
        let c0 = &comments[0];
        let AgentEvent::ReviewComment {
            reviewer,
            severity,
            path,
            line,
            body,
            id: emit_id,
        } = c0
        else {
            unreachable!("filtered to ReviewComment");
        };
        assert_eq!(emit_id, &id);
        assert_eq!(reviewer.orchestrator(), "codex");
        assert_eq!(reviewer.name(), "reviewer");
        assert_eq!(*severity, Severity::P0);
        assert_eq!(path, &PathBuf::from("internal/tui/app.go"));
        assert_eq!(*line, Some(213));
        assert_eq!(body, "Tick loop no-op");

        // Verify F-002 picked up `P1 (high)` as P1.
        let AgentEvent::ReviewComment { severity, line, .. } = &comments[1] else {
            unreachable!();
        };
        assert_eq!(*severity, Severity::P1);
        assert_eq!(*line, Some(109));

        // Verify F-003.
        let AgentEvent::ReviewComment {
            severity,
            path,
            line,
            body,
            ..
        } = &comments[2]
        else {
            unreachable!();
        };
        assert_eq!(*severity, Severity::P2);
        assert_eq!(path, &PathBuf::from("internal/tui/menu.go"));
        assert_eq!(*line, Some(44));
        assert_eq!(body, "Menu doesn't adapt");

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn rewrite_with_same_ids_does_not_reemit() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let file = impl_dir.join("impl-review-findings.md");
        let initial = "| F-101: alpha (source: codex) | P1 | a.rs:10 | NEW | — |\n\
                       | F-102: beta  (source: codex) | P2 | b.rs:20 | NEW | — |\n\
                       | F-103: gamma (source: codex) | P3 | c.rs:30 | NEW | — |\n";
        std::fs::write(&file, initial).unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_codex_findings(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let first = wait_for_count(
            &mut rx,
            |e| matches!(e, AgentEvent::ReviewComment { .. }),
            3,
            StdDuration::from_secs(3),
        )
        .await;
        assert_eq!(first.len(), 3);

        // Rewrite with identical content (same F-IDs).
        std::fs::write(&file, initial).unwrap();

        // Wait past the debounce + a healthy margin. No new comments.
        tokio::time::sleep(StdDuration::from_millis(600)).await;
        let extra: Vec<_> = drain(&mut rx)
            .into_iter()
            .filter(|e| matches!(e, AgentEvent::ReviewComment { .. }))
            .collect();
        assert!(extra.is_empty(), "expected no re-emission, got {extra:?}");

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn appending_new_finding_emits_one_more() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let file = impl_dir.join("impl-review-findings.md");
        let initial = "| F-201: a (source: codex) | P1 | a.rs:1 | NEW | — |\n\
                       | F-202: b (source: codex) | P2 | b.rs:2 | NEW | — |\n\
                       | F-203: c (source: codex) | P3 | c.rs:3 | NEW | — |\n";
        std::fs::write(&file, initial).unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_codex_findings(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let _ = wait_for_count(
            &mut rx,
            |e| matches!(e, AgentEvent::ReviewComment { .. }),
            3,
            StdDuration::from_secs(3),
        )
        .await;

        // Append a new row.
        let appended = format!("{initial}| F-204: d (source: codex) | P0 | d.rs:4 | NEW | — |\n");
        std::fs::write(&file, &appended).unwrap();

        let more = wait_for_count(
            &mut rx,
            |e| matches!(e, AgentEvent::ReviewComment { .. }),
            1,
            StdDuration::from_secs(3),
        )
        .await;
        assert_eq!(
            more.len(),
            1,
            "expected one new ReviewComment, got {more:?}"
        );
        let AgentEvent::ReviewComment { severity, path, .. } = &more[0] else {
            unreachable!();
        };
        assert_eq!(*severity, Severity::P0);
        assert_eq!(path, &PathBuf::from("d.rs"));

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn malformed_rows_do_not_panic() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let file = impl_dir.join("impl-review-findings.md");
        std::fs::write(
            &file,
            "# Review\n\
             | Finding | Severity | File | Status | Task |\n\
             |---|---|---|---|---|\n\
             not a table row at all\n\
             | only two | cells |\n\
             | F-no-digits | P1 | f.rs:1 | NEW | — |\n\
             | F-401: keeper (source: codex) | P1 | f.rs:1 | NEW | — |\n\
             | F-402: bad severity | ZZZ | g.rs:2 | NEW | — |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_codex_findings(
            tmp.path().to_path_buf(),
            id,
            tx,
            cancel.clone(),
            true,
        ));

        let comments = wait_for_count(
            &mut rx,
            |e| matches!(e, AgentEvent::ReviewComment { .. }),
            1,
            StdDuration::from_secs(2),
        )
        .await;
        assert_eq!(comments.len(), 1, "only F-401 is valid, got {comments:?}");

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    // ---- pure parser/API tests -------------------------------------------

    #[test]
    fn parse_findings_happy_path() {
        let md = "| Finding | Severity | File | Status | Task |\n\
                  |---|---|---|---|---|\n\
                  | F-001: Tick loop (source: codex) | P0 | internal/tui/app.go:213-214 | NEW | T-043 |\n\
                  | F-002: ActionOpen never handled (source: codex) | P1 (high) | internal/tui/app.go:109-174 | NEW | T-045 |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].id, "F-001");
        assert_eq!(f[0].severity, Severity::P0);
        assert_eq!(f[0].path, PathBuf::from("internal/tui/app.go"));
        assert_eq!(f[0].line, Some(213));
        assert_eq!(f[0].body, "Tick loop");
        assert_eq!(f[1].severity, Severity::P1);
        assert_eq!(f[1].line, Some(109));
    }

    #[test]
    fn parse_findings_skips_no_findings_sentinel() {
        // Shape lifted verbatim from the reviewer-emitted meta row seen
        // in production impl-review-findings.md.
        let md = "| F-043:  (source: codex) | P1 (high) | P2 (medium):LP3 (low). No issues found = output NO_FINDINGS alone. | NEW | — |\n";
        assert!(parse_findings(md).is_empty());
    }

    #[test]
    fn parse_findings_skips_empty_body() {
        let md = "| F-050: (source: codex) | P1 | foo.rs:1 | NEW | — |\n";
        assert!(parse_findings(md).is_empty());
    }

    #[test]
    fn parse_findings_path_without_line() {
        let md = "| F-010: body here (source: codex) | P2 | foo.rs | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, PathBuf::from("foo.rs"));
        assert_eq!(f[0].line, None);
    }

    #[test]
    fn parse_findings_lowercase_severity() {
        let md = "| F-020: lower (source: codex) | p3 | a.rs:5 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::P3);
    }

    #[test]
    fn parse_findings_preserves_inline_parens_before_source() {
        // A body with earlier parentheticals must only drop the trailing
        // `(source: codex)`, not the inner ones.
        let md = "| F-030: description (with aside) more text (source: codex) | P1 | x.rs:1 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].body, "description (with aside) more text");
    }

    #[test]
    fn rollup_counts_and_total() {
        // 3 P1 + 2 P2 + 1 P3 → total 6.
        let findings = vec![
            Finding {
                id: "F-1".into(),
                severity: Severity::P1,
                path: PathBuf::from("a"),
                line: None,
                body: "a".into(),
            },
            Finding {
                id: "F-2".into(),
                severity: Severity::P1,
                path: PathBuf::from("b"),
                line: None,
                body: "b".into(),
            },
            Finding {
                id: "F-3".into(),
                severity: Severity::P1,
                path: PathBuf::from("c"),
                line: None,
                body: "c".into(),
            },
            Finding {
                id: "F-4".into(),
                severity: Severity::P2,
                path: PathBuf::from("d"),
                line: None,
                body: "d".into(),
            },
            Finding {
                id: "F-5".into(),
                severity: Severity::P2,
                path: PathBuf::from("e"),
                line: None,
                body: "e".into(),
            },
            Finding {
                id: "F-6".into(),
                severity: Severity::P3,
                path: PathBuf::from("f"),
                line: None,
                body: "f".into(),
            },
        ];
        let r = rollup(&findings);
        assert_eq!(r.total, 6);
        assert_eq!(r.counts_by_severity.get(&Severity::P0), None);
        assert_eq!(r.counts_by_severity.get(&Severity::P1), Some(&3));
        assert_eq!(r.counts_by_severity.get(&Severity::P2), Some(&2));
        assert_eq!(r.counts_by_severity.get(&Severity::P3), Some(&1));
    }

    #[test]
    fn rollup_empty_is_zero() {
        let r = rollup(&[]);
        assert_eq!(r.total, 0);
        assert!(r.counts_by_severity.is_empty());
    }

    #[test]
    fn extract_first_u32_basic() {
        assert_eq!(extract_first_u32("L123-456"), Some(123));
        assert_eq!(extract_first_u32("42"), Some(42));
        assert_eq!(extract_first_u32("no digits"), None);
        assert_eq!(extract_first_u32(""), None);
    }

    #[test]
    fn parse_path_cell_variants() {
        assert_eq!(
            parse_path_cell("foo/bar.rs:L42"),
            (PathBuf::from("foo/bar.rs"), Some(42))
        );
        assert_eq!(
            parse_path_cell("foo/bar.rs:42-50"),
            (PathBuf::from("foo/bar.rs"), Some(42))
        );
        assert_eq!(
            parse_path_cell("foo/bar.rs"),
            (PathBuf::from("foo/bar.rs"), None)
        );
    }

    #[test]
    fn parse_severity_tolerates_trailing_qualifier() {
        assert_eq!(parse_severity("P1 (high)"), Some(Severity::P1));
        assert_eq!(parse_severity("p0"), Some(Severity::P0));
        assert_eq!(parse_severity("P9"), None);
        assert_eq!(parse_severity(""), None);
    }

    /// T-121: a classic `F-001 P1 src/foo.rs:42` row parses to the exact
    /// expected shape — this is the canonical happy-path example.
    #[test]
    fn parse_findings_f001_p1_row_example() {
        let md = "| F-001: race in tick loop (source: codex) | P1 | src/foo.rs:42 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "F-001");
        assert_eq!(f[0].severity, Severity::P1);
        assert_eq!(f[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(f[0].line, Some(42));
        assert_eq!(f[0].body, "race in tick loop");
    }

    /// T-121: a continuation/multiline description line that does NOT start
    /// with a pipe is silently ignored. The parser is line-oriented and a
    /// real markdown table cannot wrap, so agents should put the full
    /// description on a single row — leftover wrap lines must not crash or
    /// pollute output.
    #[test]
    fn parse_findings_multiline_continuation_ignored() {
        let md = "| F-010: primary line (source: codex) | P2 | a.rs:5 | NEW | — |\n\
                  continuation text without pipes on its own line\n\
                  | F-011: next one (source: codex) | P3 | b.rs:6 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].id, "F-010");
        assert_eq!(f[0].body, "primary line");
        assert_eq!(f[1].id, "F-011");
    }

    /// T-121: a truncated row with only two cells is silently skipped.
    #[test]
    fn parse_findings_truncated_row_skipped() {
        let md = "| F-099: truncated | P1 |\n\
                  | F-100: ok (source: codex) | P1 | ok.rs:1 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "F-100");
    }

    /// T-121: source attribution is stripped regardless of the inner vendor
    /// label (the reviewer may swap "codex" for another provider one day).
    #[test]
    fn parse_findings_strips_any_source_attribution() {
        let md = "| F-200: hello (source: claude) | P2 | a.rs:1 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].body, "hello");
    }

    /// T-121: all four severity bands (P0 / P1 / P2 / P3) are recognised in
    /// a single table, verifying the full severity-column contract.
    #[test]
    fn parse_findings_all_severity_bands() {
        let md = "| F-300: a (source: codex) | P0 | a.rs:1 | NEW | — |\n\
                  | F-301: b (source: codex) | P1 | b.rs:2 | NEW | — |\n\
                  | F-302: c (source: codex) | P2 | c.rs:3 | NEW | — |\n\
                  | F-303: d (source: codex) | P3 | d.rs:4 | NEW | — |\n";
        let f = parse_findings(md);
        assert_eq!(f.len(), 4);
        assert_eq!(f[0].severity, Severity::P0);
        assert_eq!(f[1].severity, Severity::P1);
        assert_eq!(f[2].severity, Severity::P2);
        assert_eq!(f[3].severity, Severity::P3);
    }
}
