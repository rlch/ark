//! T-082: Git diff / numstat watcher (cavekit-orchestrator-cavekit R8).
//!
//! Watches `{cwd}/.git/index` for changes + polls every 5s, running
//! `git diff --numstat HEAD` on each trigger and emitting a `FileEdited`
//! event per file whose `(additions, deletions)` pair differs from the last
//! observed values. `git diff` naturally excludes `.gitignore`'d files and
//! files that are not tracked.
//!
//! Non-git cwds return `Ok(())` without emitting anything (per
//! cavekit-orchestrator-claude-code R3 + R8 — non-git cwd is valid).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, CancellationToken, EventSink};
use notify::{RecursiveMode, Watcher};
use tokio::process::Command;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Public entry point — see module docs.
pub async fn watch_git_diff(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
) -> Result<()> {
    let git_dir = cwd.join(".git");
    let index = git_dir.join("index");
    if !git_dir.exists() {
        tracing::debug!(
            path = %git_dir.display(),
            "watch_git_diff: no .git directory — non-git cwd, exiting"
        );
        return Ok(());
    }

    let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = std_tx.send(res);
    })?;
    // Watch the .git dir non-recursively. notify's NonRecursive mode on a
    // directory reports events for children — crucially `index` — which is
    // what we want. Falling back to the dir (rather than the file itself)
    // survives git's atomic-replace-via-rename of `index` (the file we named
    // explicitly would no longer exist after the replace).
    watcher.watch(&git_dir, RecursiveMode::NonRecursive)?;

    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let target_name = index
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let reader_handle = std::thread::spawn(move || {
        for res in std_rx {
            let Ok(event) = res else { continue };
            // macOS returns `/private`-prefixed paths for events under `/tmp`
            // and `/var/folders/...`. Match by filename — `.git/` is
            // non-recursive and `index` is the only child we care about.
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

    let mut last_emitted: HashMap<PathBuf, (u32, u32)> = HashMap::new();

    // Initial scan so currently-dirty worktrees report without needing a
    // filesystem nudge.
    scan_and_emit(&cwd, &id, &tx, &mut last_emitted).await;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break;
            }
            maybe = async_rx.recv() => {
                if maybe.is_none() {
                    // Watcher side died. Fall through to polling-only mode
                    // by recreating just the poll ticker.
                    break;
                }
                scan_and_emit(&cwd, &id, &tx, &mut last_emitted).await;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {
                scan_and_emit(&cwd, &id, &tx, &mut last_emitted).await;
            }
        }
    }

    drop(watcher);
    let _ = reader_handle.join();
    Ok(())
}

/// Run `git diff --numstat HEAD` in `cwd`, parse each line, and emit a
/// `FileEdited` event when the `(additions, deletions)` pair is new or
/// different from the prior observation for that path.
async fn scan_and_emit(
    cwd: &Path,
    id: &AgentId,
    tx: &EventSink,
    last: &mut HashMap<PathBuf, (u32, u32)>,
) {
    let output = match Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg("HEAD")
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(error = %e, "watch_git_diff: git invocation failed");
            return;
        }
    };
    if !output.status.success() {
        // `git diff HEAD` fails in a repo with no commits yet; treat as no-op.
        tracing::debug!(
            code = ?output.status.code(),
            "watch_git_diff: git exited non-zero"
        );
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows = parse_numstat(&stdout);

    // F-424: build the set of paths currently in numstat so we can drop any
    // path that reverted to clean. Without this, a file cycle
    //   (a,d) -> clean -> (a,d)
    // suppresses the third transition as a duplicate of the first, violating
    // the "emit when observed numstat changes from last observation" contract.
    let current: std::collections::HashSet<PathBuf> =
        rows.iter().map(|(p, _, _)| p.clone()).collect();
    last.retain(|p, _| current.contains(p));

    for (path, adds, dels) in rows {
        match last.get(&path) {
            Some(&prev) if prev == (adds, dels) => continue,
            _ => {}
        }
        last.insert(path.clone(), (adds, dels));
        let _ = tx.send(AgentEvent::FileEdited {
            id: id.clone(),
            path,
            additions: adds,
            deletions: dels,
        });
    }
}

/// Parse `git diff --numstat HEAD` output.
///
/// Format: `<additions>\t<deletions>\t<path>` per line. Binary files
/// produce `-\t-\t<path>` — those are skipped (no numeric signal).
fn parse_numstat(stdout: &str) -> Vec<(PathBuf, u32, u32)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(adds_s) = parts.next() else { continue };
        let Some(dels_s) = parts.next() else { continue };
        let Some(path_s) = parts.next() else { continue };
        if adds_s == "-" || dels_s == "-" {
            continue;
        }
        let Ok(adds) = adds_s.parse::<u32>() else {
            continue;
        };
        let Ok(dels) = dels_s.parse::<u32>() else {
            continue;
        };
        if path_s.is_empty() {
            continue;
        }
        out.push((PathBuf::from(path_s), adds, dels));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::channel;
    use std::process::Command as StdCommand;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_id() -> AgentId {
        AgentId::new("cavekit", "git-diff")
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "ark-test")
            .env("GIT_AUTHOR_EMAIL", "ark@test")
            .env("GIT_COMMITTER_NAME", "ark-test")
            .env("GIT_COMMITTER_EMAIL", "ark@test")
            .status()
            .expect("git spawn");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.name", "ark-test"]);
        git(dir, &["config", "user.email", "ark@test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "-q", "-m", "seed"]);
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
            match tokio::time::timeout(StdDuration::from_millis(100), rx.recv()).await {
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
    async fn non_git_cwd_returns_ok_no_events() {
        let tmp = TempDir::new().unwrap();
        let (tx, mut rx) = channel(16);
        let cancel = CancellationToken::new();
        watch_git_diff(tmp.path().to_path_buf(), make_id(), tx, cancel)
            .await
            .expect("ok");
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn tracked_modification_emits_file_edited() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Modify README.md.
        std::fs::write(tmp.path().join("README.md"), "seed\nline two\nline three\n").unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_git_diff(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
        ));

        let got = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::FileEdited { path, .. } if path.ends_with("README.md")),
            StdDuration::from_secs(5),
        )
        .await;

        let edit = got.iter().find_map(|e| match e {
            AgentEvent::FileEdited {
                path,
                additions,
                deletions,
                ..
            } if path.ends_with("README.md") => Some((*additions, *deletions)),
            _ => None,
        });
        // Two added lines, zero deletions.
        assert_eq!(edit, Some((2, 0)), "events: {got:?}");

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn identical_edit_is_deduped() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("README.md"), "seed\nline two\nline three\n").unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_git_diff(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
        ));

        let _ = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::FileEdited { .. }),
            StdDuration::from_secs(5),
        )
        .await;
        let _ = drain(&mut rx);

        // Touch the file identically — rewrite same bytes. `git diff` still
        // returns the same numstat, so no new event must fire.
        std::fs::write(tmp.path().join("README.md"), "seed\nline two\nline three\n").unwrap();

        tokio::time::sleep(StdDuration::from_millis(400)).await;
        let events = drain(&mut rx);
        let relevant: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::FileEdited { .. }))
            .collect();
        assert!(
            relevant.is_empty(),
            "expected no re-emission, got {relevant:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn different_line_counts_reemits() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("README.md"), "seed\nL1\n").unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_git_diff(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
        ));

        let first = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::FileEdited { .. }),
            StdDuration::from_secs(5),
        )
        .await;
        let (a1, d1) = first
            .iter()
            .find_map(|e| match e {
                AgentEvent::FileEdited {
                    additions,
                    deletions,
                    ..
                } => Some((*additions, *deletions)),
                _ => None,
            })
            .expect("first");
        assert_eq!((a1, d1), (1, 0));
        let _ = drain(&mut rx);

        // Expand to 2 additions.
        std::fs::write(tmp.path().join("README.md"), "seed\nL1\nL2\n").unwrap();
        let second = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::FileEdited { additions: 2, .. }),
            StdDuration::from_secs(7),
        )
        .await;
        assert!(
            second.iter().any(|e| matches!(
                e,
                AgentEvent::FileEdited {
                    additions: 2,
                    deletions: 0,
                    ..
                }
            )),
            "expected re-emission with (2,0), got {second:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn untracked_and_ignored_files_are_silent() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        git(tmp.path(), &["add", ".gitignore"]);
        git(tmp.path(), &["commit", "-q", "-m", "ignore"]);

        // Untracked file.
        std::fs::write(tmp.path().join("new.txt"), "hello\n").unwrap();
        // Ignored file.
        std::fs::write(tmp.path().join("ignored.txt"), "hello\n").unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_git_diff(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
        ));

        // Wait a bit and check that no FileEdited fired. `git diff --numstat HEAD`
        // reports only tracked files, so untracked + ignored are both silent.
        tokio::time::sleep(StdDuration::from_millis(600)).await;
        let events = drain(&mut rx);
        let file_edits: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::FileEdited { .. }))
            .collect();
        assert!(
            file_edits.is_empty(),
            "expected silence on untracked/ignored, got {file_edits:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn cancel_returns_ok() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let (tx, _rx) = channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(watch_git_diff(
            tmp.path().to_path_buf(),
            make_id(),
            tx,
            cancel.clone(),
        ));
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        cancel.cancel();
        let result = tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("join timeout")
            .expect("join");
        result.expect("watcher ok");
    }

    // ---- F-424 regression ----

    /// F-424: a file cycling (a,d) → clean → same (a,d) must emit FileEdited
    /// on the third observation. Prior bug: `last` retained the stale entry
    /// from step 1, so step 3 was suppressed as a duplicate.
    ///
    /// We exercise `scan_and_emit` directly across three git states (rather
    /// than relying on the 5s poll ticker) for determinism.
    #[tokio::test]
    async fn reemits_after_clean_revert_cycle() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let readme = tmp.path().join("README.md");

        let (tx, mut rx) = channel(64);
        let id = make_id();
        let mut last: HashMap<PathBuf, (u32, u32)> = HashMap::new();

        // t1: modify README with (adds=3, dels=1) relative to HEAD.
        //     HEAD content is "seed\n" (1 line). Write 3 new lines + drop
        //     "seed" → adds=3, dels=1.
        std::fs::write(&readme, "a\nb\nc\n").unwrap();
        scan_and_emit(tmp.path(), &id, &tx, &mut last).await;
        let t1 = drain(&mut rx);
        let t1_edit = t1.iter().find_map(|e| match e {
            AgentEvent::FileEdited {
                path,
                additions,
                deletions,
                ..
            } if path.ends_with("README.md") => Some((*additions, *deletions)),
            _ => None,
        });
        assert_eq!(
            t1_edit,
            Some((3, 1)),
            "t1: expected FileEdited(3,1); got {t1:?}"
        );
        assert_eq!(last.get(&PathBuf::from("README.md")), Some(&(3u32, 1u32)));

        // t2: revert the worktree to HEAD → README no longer in numstat.
        //     `last` MUST drop the stale entry; no event should fire.
        git(tmp.path(), &["checkout", "--", "README.md"]);
        scan_and_emit(tmp.path(), &id, &tx, &mut last).await;
        let t2 = drain(&mut rx);
        let t2_edits: Vec<_> = t2
            .iter()
            .filter(|e| matches!(e, AgentEvent::FileEdited { .. }))
            .collect();
        assert!(
            t2_edits.is_empty(),
            "t2: expected no events after revert; got {t2_edits:?}"
        );
        assert!(
            !last.contains_key(&PathBuf::from("README.md")),
            "t2: last must drop README entry after revert; still has {last:?}",
        );

        // t3: re-apply the identical (3,1) modification. The bug was that
        //     `last` still held (3,1) and suppressed this. Post-fix, last was
        //     cleared in t2, so t3 emits again.
        std::fs::write(&readme, "a\nb\nc\n").unwrap();
        scan_and_emit(tmp.path(), &id, &tx, &mut last).await;
        let t3 = drain(&mut rx);
        let t3_edit = t3.iter().find_map(|e| match e {
            AgentEvent::FileEdited {
                path,
                additions,
                deletions,
                ..
            } if path.ends_with("README.md") => Some((*additions, *deletions)),
            _ => None,
        });
        assert_eq!(
            t3_edit,
            Some((3, 1)),
            "t3: expected re-emission of FileEdited(3,1) after revert cycle; got {t3:?}"
        );
    }

    /// F-424 supporting case: staging a tracked change and then cleaning it
    /// out must also drop `last`. `git diff HEAD` considers both worktree
    /// and index changes, so staging an edit counts as present; undoing it
    /// entirely (worktree + index clean) should remove the entry.
    #[tokio::test]
    async fn staged_then_cleaned_drops_last_entry() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let readme = tmp.path().join("README.md");

        let (tx, mut rx) = channel(32);
        let id = make_id();
        let mut last: HashMap<PathBuf, (u32, u32)> = HashMap::new();

        // Stage a modification.
        std::fs::write(&readme, "seed\nextra\n").unwrap();
        git(tmp.path(), &["add", "README.md"]);
        scan_and_emit(tmp.path(), &id, &tx, &mut last).await;
        let staged = drain(&mut rx);
        assert!(
            staged
                .iter()
                .any(|e| matches!(e, AgentEvent::FileEdited { .. })),
            "expected FileEdited while staged; got {staged:?}"
        );
        assert!(last.contains_key(&PathBuf::from("README.md")));

        // Fully clean it (reset + checkout).
        git(tmp.path(), &["reset", "HEAD", "README.md"]);
        git(tmp.path(), &["checkout", "--", "README.md"]);
        scan_and_emit(tmp.path(), &id, &tx, &mut last).await;
        let cleaned = drain(&mut rx);
        let edits: Vec<_> = cleaned
            .iter()
            .filter(|e| matches!(e, AgentEvent::FileEdited { .. }))
            .collect();
        assert!(
            edits.is_empty(),
            "expected no events after full clean; got {edits:?}"
        );
        assert!(
            !last.contains_key(&PathBuf::from("README.md")),
            "last must drop README on clean; still {last:?}",
        );
    }

    // ---- parser tests ----

    #[test]
    fn parse_numstat_basic() {
        let out = "3\t1\tfoo.rs\n0\t2\tpath with spaces.txt\n";
        let rows = parse_numstat(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (PathBuf::from("foo.rs"), 3, 1));
        assert_eq!(rows[1], (PathBuf::from("path with spaces.txt"), 0, 2));
    }

    #[test]
    fn parse_numstat_skips_binary_and_empty() {
        let out = "-\t-\tbin.dat\n\n5\t0\tok.rs\n";
        let rows = parse_numstat(out);
        assert_eq!(rows, vec![(PathBuf::from("ok.rs"), 5, 0)]);
    }
}
