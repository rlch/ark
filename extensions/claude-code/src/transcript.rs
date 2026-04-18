//! Transcript fs-watcher + tail cursor primitives (T-007 salvage; R8).
//!
//! The pre-2026-04-18 `crates/orchestrators/claude-code/src/lib.rs` was
//! in the middle of a methodology-layer rewrite when it was stubbed;
//! the `notify`-based transcript-tail bits that R8 needs never made it
//! into the stub body. This module salvages the *shape* of that
//! planned code — a small, pull-based tail API backed by `notify` —
//! without bringing along any orchestrator-trait surface. The
//! extension owns its own lifecycle per the 2026-04-18 pivot (kit
//! §Non-goals "no orchestrator trait").
//!
//! ## Design
//!
//! Two primitives:
//!
//! - [`TailCursor`] — byte-offset cursor over a single JSONL transcript
//!   file. [`TailCursor::poll_new_lines`] opens the file, seeks to the
//!   cursor, and returns any full lines appended since the last poll,
//!   advancing the cursor atomically. Survives truncation (Claude Code
//!   rotates transcripts on compaction): if the file shrinks below the
//!   cursor, the cursor is reset to 0 and the file is re-read from the
//!   top. Missing file → empty vec, cursor untouched.
//!
//! - [`TranscriptWatcher`] — thin wrapper around a `notify`
//!   recursive watcher on a transcript directory (one per session).
//!   Callers start the watcher, register `TailCursor`s per interested
//!   file, and poll the `TailCursor` on each `TranscriptEvent` tick.
//!   This is the "pull model, not push" R8 calls out: events are
//!   hints to re-poll, not full tail payloads.
//!
//! Per R8 acceptance:
//!
//! - New file appearance under `.../subagents/` emits a
//!   [`TranscriptEvent::FileAppeared`] — no ExtEvent is fired. The
//!   authoritative subagent-lifecycle source is the `SubagentStart`
//!   hook via cc-hook (R3).
//! - Watcher survives transcript truncation (handled inside
//!   [`TailCursor::poll_new_lines`]).
//! - Missing transcript directory at session start: [`start_watcher`]
//!   logs a warn and returns early with a watcher that never ticks
//!   until the caller re-invokes it. T-011 / T-020 will re-register
//!   once the directory appears (Claude Code creates it on first
//!   hook event).
//!
//! ## What we did NOT salvage
//!
//! - `ClaudeCodeOrchestrator` struct, `Orchestrator` impl, `detect` —
//!   all orchestrator-trait surface. Per kit §Non-goals the
//!   extension owns its own lifecycle; the orchestrator concept is
//!   deprecated in soul Phase 1+.
//! - `ClaudeCodeConfig` struct keyed off `SessionSpec.ext_config` —
//!   the v3 config schema lands in T-026+ (R9) against the
//!   `ArkExtension` config surface, not salvaged here.

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use notify::{RecursiveMode, Watcher};
use tracing::{debug, warn};

/// Coarse signal emitted by a [`TranscriptWatcher`] when the OS reports
/// a filesystem change under the watched directory. The payload is
/// intentionally thin — callers re-poll the relevant [`TailCursor`]
/// rather than trusting notify's event granularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// A path under the watched tree changed (modify / create / any).
    /// The `path` is whatever `notify` reported; callers correlate by
    /// file name rather than by exact path because macOS FSEvents
    /// returns `/private`-prefixed canonical paths that don't `==`
    /// the watched prefix.
    Changed(PathBuf),
    /// A new file appeared directly under `<dir>/subagents/`. Pattern-
    /// specific hint for R6 fan-out UIs; NOT an authoritative
    /// lifecycle signal (that is the `SubagentStart` hook per R3).
    FileAppeared(PathBuf),
}

/// Byte-offset cursor over a single JSONL transcript file.
///
/// Safe to poll repeatedly — each [`poll_new_lines`][Self::poll_new_lines]
/// call returns only full lines appended since the last successful
/// poll. Partial trailing lines (no `\n`) are left in the file; the
/// cursor advances only to the last newline byte boundary, so a
/// concurrent writer's half-written line is never returned.
///
/// Truncation detection uses either a shrunk length (new len < cursor
/// offset) OR a changed inode (Claude Code's compaction rotates files
/// in-place, so the file's identity flips even when the new file's
/// byte length exceeds the cursor). Both paths reset the cursor to 0
/// and re-read from the top.
pub struct TailCursor {
    path: PathBuf,
    offset: u64,
    /// Unix inode of the file as observed on the last successful poll.
    /// `None` before the first observation. Changed inode → rotation.
    #[cfg(unix)]
    inode: Option<u64>,
}

impl TailCursor {
    /// Build a cursor starting at byte `0` of `path`. The file does not
    /// need to exist yet — the first [`poll_new_lines`][Self::poll_new_lines]
    /// that finds a missing file is a no-op.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            #[cfg(unix)]
            inode: None,
        }
    }

    /// Build a cursor starting at the current end-of-file. Useful when
    /// the caller only wants to see content written *after* a given
    /// point (e.g. session start) and doesn't want the initial backlog.
    pub fn at_end_of(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (offset, inode) = match fs::metadata(&path) {
            Ok(m) => (m.len(), current_inode(&m)),
            Err(_) => (0, None),
        };
        Self {
            path,
            offset,
            #[cfg(unix)]
            inode,
        }
    }

    /// Watched path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Byte offset the next poll will read from.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Read every full line appended since the last poll.
    ///
    /// On truncation (file length < cursor) the cursor resets to 0 and
    /// the whole file is re-read — Claude Code may rotate the
    /// transcript on compaction (R8).
    ///
    /// Missing file → `Ok(vec![])`, cursor untouched. Any other IO
    /// error propagates so the caller can log and decide.
    pub fn poll_new_lines(&mut self) -> std::io::Result<Vec<String>> {
        let meta = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = meta.len();
        let new_inode = current_inode(&meta);

        // Truncation / rotation detection:
        // - length shrank below the cursor → compaction-in-place.
        // - inode flipped → rotation via a fresh file at the same path.
        // Either path forces the cursor to 0.
        let rotated_inode = {
            #[cfg(unix)]
            {
                matches!((self.inode, new_inode), (Some(old), Some(new)) if old != new)
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        if len < self.offset || rotated_inode {
            debug!(
                path = %self.path.display(),
                prev_offset = self.offset,
                new_len = len,
                rotated_inode = rotated_inode,
                "transcript truncated/rotated; resetting cursor"
            );
            self.offset = 0;
        }
        #[cfg(unix)]
        {
            self.inode = new_inode;
        }
        if len == self.offset {
            return Ok(Vec::new());
        }

        let mut f = fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(f);
        let mut lines = Vec::new();
        let mut consumed: u64 = 0;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            if !line.ends_with('\n') {
                // Partial trailing line — don't consume it, leave the
                // cursor at the start of the partial so the next poll
                // re-reads the whole line once the writer flushes `\n`.
                break;
            }
            consumed += n as u64;
            // Strip the trailing newline to make downstream JSONL
            // parsing straightforward; callers that need the raw byte
            // sequence can reconstruct.
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            lines.push(line);
        }
        self.offset += consumed;
        Ok(lines)
    }
}

/// Thin wrapper around a `notify::RecommendedWatcher` watching a
/// transcript directory.
///
/// Drop the value to stop watching. Events arrive on the receiver
/// returned by [`start_watcher`].
pub struct TranscriptWatcher {
    _watcher: notify::RecommendedWatcher,
    dir: PathBuf,
}

impl TranscriptWatcher {
    /// Directory the watcher was registered on.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Start watching `dir` recursively for transcript changes. Returns
/// the watcher (keep it alive — dropping stops the watch) and a
/// receiver of [`TranscriptEvent`]s.
///
/// Missing directory → logs a warn and returns `Ok(None)`. Callers
/// re-invoke when the directory appears (Claude Code creates it on
/// first hook event).
pub fn start_watcher(
    dir: impl AsRef<Path>,
) -> std::io::Result<Option<(TranscriptWatcher, mpsc::Receiver<TranscriptEvent>)>> {
    let dir = dir.as_ref().to_path_buf();
    if !dir.exists() {
        warn!(
            dir = %dir.display(),
            "transcript dir missing at watcher start; caller must retry when it appears"
        );
        return Ok(None);
    }

    let (tx, rx) = mpsc::channel::<TranscriptEvent>();
    let tx_outer = tx.clone();
    let subagents_dir = dir.join("subagents");
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        for path in event.paths.iter() {
            // R8 pattern-specific hint: new file directly under
            // `<dir>/subagents/` is `FileAppeared`. Anything else is
            // a coarse `Changed`.
            let appeared = matches!(
                event.kind,
                notify::EventKind::Create(_)
                    | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
            ) && path.parent() == Some(subagents_dir.as_path());
            let ev = if appeared {
                TranscriptEvent::FileAppeared(path.clone())
            } else {
                TranscriptEvent::Changed(path.clone())
            };
            // Receiver dropped means the watcher is being torn down —
            // stop emitting further events.
            if tx_outer.send(ev).is_err() {
                break;
            }
        }
    })
    .map_err(notify_to_io_err)?;
    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .map_err(notify_to_io_err)?;

    Ok(Some((
        TranscriptWatcher {
            _watcher: watcher,
            dir,
        },
        rx,
    )))
}

/// Encode a working directory into the Claude Code transcript folder
/// name convention.
///
/// Claude Code stores transcripts under
/// `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`; the
/// encoded-cwd is the absolute cwd with `/` replaced by `-` and a
/// leading `~/` prefix stripped (the exact encoding is verified
/// against real claude output at R8 implementation time — this helper
/// applies the documented approximation).
///
/// Kept here rather than inlined into the watcher so tests can pin
/// the encoding and the view layer (R6) can share it.
pub fn encode_cwd(cwd: &Path) -> String {
    let s = cwd.display().to_string();
    let trimmed = s.strip_prefix("~/").unwrap_or(&s);
    // Leading slash becomes a leading dash so the first segment is
    // preserved (e.g. `/Users/x/repo` → `-Users-x-repo`), matching the
    // observed claude output.
    trimmed.replace('/', "-")
}

fn notify_to_io_err(e: notify::Error) -> std::io::Error {
    std::io::Error::other(format!("notify: {e}"))
}

/// Resolve the inode of a stat'd file on unix, `None` on non-unix
/// platforms (inode-based rotation detection is best-effort).
#[cfg(unix)]
fn current_inode(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn current_inode(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::OpenOptions;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn write_line(path: &Path, line: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        writeln!(f, "{line}").expect("write");
        f.flush().ok();
    }

    #[test]
    fn tail_cursor_returns_empty_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let mut c = TailCursor::new(tmp.path().join("nope.jsonl"));
        let lines = c.poll_new_lines().expect("ok");
        assert!(lines.is_empty());
        assert_eq!(c.offset(), 0);
    }

    #[test]
    fn tail_cursor_reads_full_lines_and_advances() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        write_line(&path, "one");
        write_line(&path, "two");
        let mut c = TailCursor::new(&path);
        let lines = c.poll_new_lines().expect("ok");
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        assert!(c.offset() > 0);

        // Second poll with no writes → empty, cursor steady.
        let prev = c.offset();
        assert!(c.poll_new_lines().expect("ok").is_empty());
        assert_eq!(c.offset(), prev);

        // Append + re-poll → only the new line.
        write_line(&path, "three");
        let lines = c.poll_new_lines().expect("ok");
        assert_eq!(lines, vec!["three".to_string()]);
    }

    #[test]
    fn tail_cursor_leaves_partial_trailing_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        // Partial line without trailing newline.
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"partial").unwrap();
        f.flush().ok();

        let mut c = TailCursor::new(&path);
        assert!(c.poll_new_lines().expect("ok").is_empty());
        assert_eq!(c.offset(), 0, "offset must not advance over a partial line");

        // Finish the line + write a second. Both should come back.
        f.write_all(b" done\ntwo\n").unwrap();
        f.flush().ok();
        let lines = c.poll_new_lines().expect("ok");
        assert_eq!(lines, vec!["partial done".to_string(), "two".to_string()]);
    }

    #[test]
    fn tail_cursor_resets_on_shrink() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        write_line(&path, "pre-rotate-longer-line");
        let mut c = TailCursor::new(&path);
        assert_eq!(c.poll_new_lines().expect("ok").len(), 1);

        // Shrink the file to less than the current cursor and write a
        // shorter line — length-based truncation detection fires.
        fs::write(&path, "short\n").unwrap();
        let lines = c.poll_new_lines().expect("ok");
        assert_eq!(lines, vec!["short".to_string()]);
    }

    #[test]
    #[cfg(unix)]
    fn tail_cursor_resets_on_rotation_by_inode() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        write_line(&path, "pre-rotate");
        let mut c = TailCursor::new(&path);
        assert_eq!(c.poll_new_lines().expect("ok").len(), 1);

        // Rotate by unlink + create: a new inode at the same path means
        // Claude Code has started a fresh transcript even if the new
        // byte length exceeds the cursor.
        fs::remove_file(&path).unwrap();
        write_line(&path, "after-rotate-with-extra-length");
        let lines = c.poll_new_lines().expect("ok");
        assert_eq!(lines, vec!["after-rotate-with-extra-length".to_string()]);
    }

    #[test]
    fn start_watcher_missing_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        let got = start_watcher(tmp.path().join("does-not-exist")).expect("ok");
        assert!(got.is_none());
    }

    #[test]
    fn start_watcher_emits_event_on_write() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let (_w, rx) = start_watcher(dir).expect("ok").expect("dir present");
        // Give notify a moment to register.
        thread::sleep(Duration::from_millis(50));
        write_line(&dir.join("t.jsonl"), "hello");
        // Drain with a short deadline — we care that at least one
        // event came through, not the exact kind.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got = false;
        while std::time::Instant::now() < deadline {
            if rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                got = true;
                break;
            }
        }
        assert!(got, "expected a TranscriptEvent within 2s");
    }

    #[test]
    fn encode_cwd_replaces_slashes_with_dashes() {
        assert_eq!(encode_cwd(Path::new("/Users/rjm/repo")), "-Users-rjm-repo");
        assert_eq!(encode_cwd(Path::new("~/work")), "work");
    }
}
