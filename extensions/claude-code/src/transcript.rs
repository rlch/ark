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
use std::sync::{Arc, Mutex};

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, info, warn};

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

/// T-025 plan-name alias for [`TailCursor`]. The build site names the
/// type `TranscriptTail` (shared across R6 views + R11 list columns);
/// the salvaged T-007 implementation already lives on [`TailCursor`]
/// with identical semantics (byte-offset cursor, truncation / rotation
/// handling, missing-file → empty-vec). The alias keeps downstream
/// view-tier code (T-036) free to write `TranscriptTail::new(path)` as
/// the plan reads while preserving the existing test surface.
pub type TranscriptTail = TailCursor;

// ---------------------------------------------------------------------------
// T-027 — `notify`-based session transcript-dir watcher
// ---------------------------------------------------------------------------

/// Internal-wire event emitted by [`TranscriptDirWatcher`].
///
/// Per R8, the subagent-transcript file-creation signal is NOT promoted
/// to a core-bus [`ark_types::ExtEvent`] — the authoritative subagent
/// lifecycle source is the `SubagentStart` hook via cc-hook (T-037).
/// This channel is a thin internal wiring surface that view consumers
/// (T-035 / T-036) subscribe to when they want the raw filesystem
/// heads-up. For Tier 5 the receiver end is drained by a log-only
/// debug sink — real consumers land with views in Tier 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptDirEvent {
    /// A new file appeared directly under `<dir>/subagents/`. Carries
    /// the absolute path notify reported. NO ExtEvent is derived from
    /// this — views may use it to eagerly open a [`TranscriptTail`] on
    /// the file, but the authoritative subagent lifecycle signal is the
    /// `SubagentStart` hook (R8 acceptance criterion).
    SubagentTranscriptCreated(PathBuf),
    /// A modify / create / any-kind fs event landed under the tracked
    /// directory (or its `subagents/` subdir) for a path that is NOT a
    /// new file directly under `subagents/`. Callers correlate by file
    /// name to decide whether to re-poll a cached [`TranscriptTail`].
    TranscriptModified(PathBuf),
}

/// T-027 + T-028 — recursive `notify` watcher over the active session's
/// transcript directory + its `subagents/` subdirectory.
///
/// Owns a `notify::RecommendedWatcher` and a `tokio::sync::mpsc`
/// sender. [`Self::ensure_tracking`] is the only public mutator; it
/// derives the tracked dir from the `transcript_path.parent()` of the
/// first cc-hook payload (per decisions doc R-14 the payload carries
/// an absolute transcript path — no encoding probe needed). The call
/// is idempotent: re-invoking with the same parent is a no-op;
/// re-invoking with a different parent replaces the prior watch.
///
/// Missing-directory semantics (T-028 acceptance):
///  - `ensure_tracking(dir)` on a dir that does not yet exist logs
///    `awaiting transcript dir` at INFO + leaves the watcher in the
///    "no active watch" state. The extension does NOT error.
///  - Callers re-invoke `ensure_tracking` when a later payload signals
///    the dir now exists (Claude Code creates it on first session
///    event) — this works because every hook fire routes through the
///    accept loop which calls `ensure_tracking` unconditionally.
pub struct TranscriptDirWatcher {
    /// Keeps the notify watcher alive; replaced wholesale when
    /// `ensure_tracking` switches directories. `None` when no
    /// directory is being watched (e.g. `ensure_tracking` was called
    /// for a dir that did not yet exist).
    watcher: Option<notify::RecommendedWatcher>,
    /// Path of the currently-watched parent dir; `None` until the
    /// first successful `ensure_tracking`.
    tracked_dir: Option<PathBuf>,
    /// Sender half of the internal event channel. Cloned into the
    /// notify event-handler closure; receivers are Tier 6 views.
    tx: tokio_mpsc::UnboundedSender<TranscriptDirEvent>,
}

impl std::fmt::Debug for TranscriptDirWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptDirWatcher")
            .field("tracked_dir", &self.tracked_dir)
            .field("watcher_active", &self.watcher.is_some())
            .finish()
    }
}

impl TranscriptDirWatcher {
    /// Build a fresh watcher with no active subscription. Returns the
    /// watcher + the receiver end of the internal event channel. The
    /// watcher itself is not `Clone` — wrap in `Arc<Mutex<…>>` if
    /// multiple publishers need to hand it around (the extension does
    /// exactly this so the accept-loop task can call `ensure_tracking`
    /// on each incoming hook frame).
    pub fn new() -> (Self, tokio_mpsc::UnboundedReceiver<TranscriptDirEvent>) {
        let (tx, rx) = tokio_mpsc::unbounded_channel();
        (
            Self {
                watcher: None,
                tracked_dir: None,
                tx,
            },
            rx,
        )
    }

    /// Current tracked directory, if any. `None` before the first
    /// successful `ensure_tracking`, or after an `ensure_tracking` that
    /// targetted a missing dir (T-028 "watcher logs + waits").
    pub fn tracked_dir(&self) -> Option<&Path> {
        self.tracked_dir.as_deref()
    }

    /// Ensure a recursive notify watch is active on `dir`. Idempotent:
    ///
    ///  - Already watching `dir` → no-op.
    ///  - Watching a different dir → drop old watcher + bind new one.
    ///  - `dir` does not exist → log INFO `awaiting transcript dir` +
    ///    clear any prior watch; subsequent call once the dir appears
    ///    installs the watch (T-028).
    ///  - notify registration errored → log WARN + leave prior watch
    ///    in place. The extension never fails a session on watcher
    ///    trouble (fail-open philosophy from R2 + R8).
    pub fn ensure_tracking(&mut self, dir: &Path) {
        let dir_owned = dir.to_path_buf();

        if self.tracked_dir.as_deref() == Some(dir) && self.watcher.is_some() {
            debug!(
                dir = %dir.display(),
                "claude-code: transcript dir watcher already tracking"
            );
            return;
        }

        if !dir.exists() {
            info!(
                dir = %dir.display(),
                "claude-code: awaiting transcript dir; will bind once it appears"
            );
            // Drop any prior watcher + clear the tracked dir so a
            // later ensure_tracking call (same or different dir)
            // re-evaluates from scratch.
            self.watcher = None;
            self.tracked_dir = None;
            return;
        }

        // notify handler closure — runs on notify's internal thread.
        // Clones the tx so the closure owns a sender independent of
        // `self.tx` (watcher may outlive the lock-holder across
        // replacements).
        let tx = self.tx.clone();
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            for path in event.paths.iter() {
                // "New file under `<dir>/subagents/`" = the subagent-
                // transcript-created signal. We correlate by the
                // penultimate path component rather than exact parent
                // equality because macOS FSEvents reports
                // `/private`-prefixed canonical paths that don't `==`
                // the watched prefix (same quirk covered in the
                // legacy `start_watcher` above).
                let parent_leaf = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str());
                let is_create = matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                );
                let ev = if is_create && parent_leaf == Some("subagents") {
                    TranscriptDirEvent::SubagentTranscriptCreated(path.clone())
                } else {
                    TranscriptDirEvent::TranscriptModified(path.clone())
                };
                // Receivers gone = loop torn down; stop emitting.
                if tx.send(ev).is_err() {
                    break;
                }
            }
        };

        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(e) => {
                warn!(
                    dir = %dir.display(),
                    error = %e,
                    "claude-code: notify watcher construction failed; skipping tracking"
                );
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir_owned, RecursiveMode::Recursive) {
            warn!(
                dir = %dir.display(),
                error = %e,
                "claude-code: notify recursive watch failed; skipping tracking"
            );
            return;
        }

        // A recursive watch on `dir` already covers `dir/subagents`
        // even when that subdir doesn't exist yet (notify catches the
        // subdir creation + its contents because of RecursiveMode).
        // We leave an explicit best-effort watch on `subagents/`
        // intentionally OFF — the recursive parent watch is
        // authoritative and double-watching causes duplicate events
        // on some platforms.

        debug!(
            dir = %dir.display(),
            "claude-code: transcript dir watcher bound"
        );
        self.watcher = Some(watcher);
        self.tracked_dir = Some(dir_owned);
    }
}

/// Spawn a log-only Tier-5 consumer of [`TranscriptDirEvent`]s. Real
/// view-tier consumers land in T-035 / T-036; this sink mirrors the
/// Tier 2 "wire it up but just log for now" pattern so the channel is
/// drained and the watcher never wedges on a full unbounded buffer.
///
/// Returns the spawned task's `JoinHandle` so the caller can abort it
/// at session end. The task exits cleanly when every sender is
/// dropped.
pub fn spawn_log_sink(
    mut rx: tokio_mpsc::UnboundedReceiver<TranscriptDirEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                TranscriptDirEvent::SubagentTranscriptCreated(path) => {
                    debug!(
                        path = %path.display(),
                        "claude-code: subagent transcript file created (watcher)"
                    );
                }
                TranscriptDirEvent::TranscriptModified(path) => {
                    debug!(
                        path = %path.display(),
                        "claude-code: transcript modified (watcher)"
                    );
                }
            }
        }
    })
}

/// Arc-wrapped watcher handle held by the extension.
///
/// The extension clones the handle into the socket accept-loop task so
/// each incoming cc-hook frame can invoke [`TranscriptDirWatcher::ensure_tracking`]
/// with the parent of its `transcript_path`. Idempotency is handled
/// inside `ensure_tracking` — the Mutex merely serialises transitions.
pub type SharedDirWatcher = Arc<Mutex<TranscriptDirWatcher>>;

/// Extract `transcript_path` from a cc-hook payload's `extra` bag, if
/// present + absolute + a string-shaped JSON value. Returns the
/// **parent directory** — the thing [`TranscriptDirWatcher::ensure_tracking`]
/// consumes — not the file itself.
///
/// Per decisions doc R-14 (kit R8) the hook payload carries an
/// absolute `transcript_path` on every fire; this helper simply
/// surfaces the parent dir for watcher binding. Missing key, wrong
/// type, relative path, or no parent → `None`; caller treats as "no
/// watcher binding this fire", not as an error.
pub fn extract_transcript_parent_from_payload(
    extra: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<PathBuf> {
    let raw = extra.get("transcript_path")?.as_str()?;
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    path.parent().map(PathBuf::from)
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

    // -----------------------------------------------------------------------
    // T-027 / T-028 — TranscriptDirWatcher unit coverage
    // -----------------------------------------------------------------------

    #[test]
    fn transcript_tail_is_tail_cursor_alias() {
        // T-025: plan-name alias. This pin guards against a refactor
        // that accidentally splits the two types — downstream view
        // code (T-036) writes `TranscriptTail::new(path)` expecting
        // the cursor semantics proven out above.
        let tmp = TempDir::new().unwrap();
        let mut t: TranscriptTail = TranscriptTail::new(tmp.path().join("no.jsonl"));
        assert!(t.poll_new_lines().expect("ok").is_empty());
    }

    #[test]
    fn extract_parent_from_payload_accepts_absolute() {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert(
            "transcript_path".to_string(),
            serde_json::json!("/tmp/projects/abc/session.jsonl"),
        );
        let got = extract_transcript_parent_from_payload(&extra).expect("some");
        assert_eq!(got, PathBuf::from("/tmp/projects/abc"));
    }

    #[test]
    fn extract_parent_from_payload_rejects_relative_and_missing() {
        let empty = std::collections::BTreeMap::new();
        assert!(extract_transcript_parent_from_payload(&empty).is_none());

        let mut bad_type = std::collections::BTreeMap::new();
        bad_type.insert(
            "transcript_path".to_string(),
            serde_json::json!({"nope": true}),
        );
        assert!(extract_transcript_parent_from_payload(&bad_type).is_none());

        let mut relative = std::collections::BTreeMap::new();
        relative.insert(
            "transcript_path".to_string(),
            serde_json::json!("projects/abc/session.jsonl"),
        );
        assert!(extract_transcript_parent_from_payload(&relative).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dir_watcher_missing_dir_leaves_no_active_watch() {
        // T-028 acceptance: watcher logs + waits when the dir is
        // missing. Here we just assert the no-active-watch invariant;
        // the log assertion lives in the integration test which uses
        // a scoped tracing subscriber.
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("projects").join("abc");
        let (mut w, _rx) = TranscriptDirWatcher::new();
        w.ensure_tracking(&missing);
        assert!(
            w.tracked_dir().is_none(),
            "missing dir must not install a watch"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dir_watcher_binds_on_existing_dir_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let (mut w, _rx) = TranscriptDirWatcher::new();
        w.ensure_tracking(&dir);
        assert_eq!(w.tracked_dir(), Some(dir.as_path()));

        // Second call on same dir: idempotent no-op (watcher stays
        // bound; tracked_dir unchanged).
        w.ensure_tracking(&dir);
        assert_eq!(w.tracked_dir(), Some(dir.as_path()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dir_watcher_emits_subagent_transcript_created() {
        // T-027 acceptance: new file under `<dir>/subagents/` is an
        // internal `SubagentTranscriptCreated` wire event (NOT an
        // ExtEvent — R8 authoritative lifecycle is the hook).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("subagents")).unwrap();

        let (mut w, mut rx) = TranscriptDirWatcher::new();
        w.ensure_tracking(&dir);

        // Notify needs a moment to register.
        tokio::time::sleep(Duration::from_millis(50)).await;

        write_line(&dir.join("subagents").join("agent-123.jsonl"), "hello");

        // Poll-with-timeout (macOS FSEvents has variable latency).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw_subagent = false;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
                Ok(Some(TranscriptDirEvent::SubagentTranscriptCreated(path))) => {
                    assert!(path.file_name().unwrap() == "agent-123.jsonl");
                    saw_subagent = true;
                    break;
                }
                Ok(Some(TranscriptDirEvent::TranscriptModified(_))) => {
                    // Coarse create/modify on the parent path — keep
                    // draining until we see the subagent-specific one
                    // (notify ordering varies per platform).
                    continue;
                }
                Ok(None) => break,  // channel closed
                Err(_) => continue, // timeout — keep draining
            }
        }
        assert!(
            saw_subagent,
            "expected a SubagentTranscriptCreated event within 3s"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dir_watcher_reacts_once_missing_dir_appears() {
        // T-028 acceptance: watcher logs + waits when dir is missing,
        // reacts when it appears. Here we drive the "dir created
        // later" path through two `ensure_tracking` invocations (the
        // accept-loop calls ensure_tracking on every hook frame, so
        // idempotency + late-bind is the natural wiring).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("projects").join("abc");

        let (mut w, mut rx) = TranscriptDirWatcher::new();
        w.ensure_tracking(&dir);
        assert!(w.tracked_dir().is_none(), "missing dir: no watch");

        // Claude Code creates the dir.
        std::fs::create_dir_all(&dir).unwrap();

        // Next hook frame: accept-loop re-invokes ensure_tracking.
        w.ensure_tracking(&dir);
        assert_eq!(w.tracked_dir(), Some(dir.as_path()));

        // Now a transcript write lands an event.
        tokio::time::sleep(Duration::from_millis(50)).await;
        write_line(&dir.join("session.jsonl"), "payload");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw = false;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
                Ok(Some(_)) => {
                    saw = true;
                    break;
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(saw, "expected an event after late-bind");
    }
}
