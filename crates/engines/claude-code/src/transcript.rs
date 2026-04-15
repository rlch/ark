//! Claude Code transcript tailer (cavekit-engine-claude-code R2).
//!
//! Tails `~/.claude/projects/{encoded_cwd}/{session_id}.jsonl` line-by-line,
//! parses each JSON record into typed [`AgentEvent`] values, and pushes them
//! onto the shared event bus.
//!
//! ## Encoded-cwd assumption
//!
//! Claude Code stores per-project transcripts under
//! `~/.claude/projects/<encoded-cwd>/`. Empirically the encoding is
//! "replace every `/` with `-`" applied to the absolute cwd, so e.g.
//! `/Users/rjm/Coding/foo` becomes `-Users-rjm-Coding-foo`. We could not find
//! authoritative documentation. To make this overridable for tests and for
//! future schema drift, the parent `projects/` directory may be overridden via
//! the `ARK_CLAUDE_PROJECTS_DIR` environment variable.
//!
//! ## Lifecycle
//!
//! The tailer:
//! 1. resolves the transcript path (no-op if `config_gate` is false),
//! 2. retries up to ~10×200ms while the file does not yet exist,
//! 3. opens it, replays existing contents, then watches for appends via
//!    [`notify`],
//! 4. parses each newline-delimited JSON record, emits derived events,
//! 5. exits on cancellation or a fatal IO error (returns `Ok(())` on the
//!    fail-open paths so the supervisor keeps the run alive — diagnostics
//!    are surfaced via `tracing::warn`).
//!
//! Internal session_id rotation detection is out of scope for v1 — the
//! caller is expected to spawn a fresh task on each `SessionStart` hook.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use ark_types::{AgentEvent, AgentId, EventSink, MessageRole};
use notify::{RecursiveMode, Watcher};
use serde_json::Value;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader, SeekFrom};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const SUMMARY_MAX_CHARS: usize = 80;
const FILE_NOT_FOUND_RETRIES: usize = 10;
const FILE_NOT_FOUND_BACKOFF: Duration = Duration::from_millis(200);

/// Encode an absolute cwd into Claude Code's project-directory naming scheme.
///
/// The encoding replaces every `/` with `-`. Absolute paths therefore start
/// with `-` (e.g. `/Users/rjm/foo` → `-Users-rjm-foo`). Returns `None` if the
/// cwd is not valid UTF-8 (Claude itself can't represent such paths either).
pub fn encode_cwd(cwd: &Path) -> Option<String> {
    let s = cwd.to_str()?;
    Some(s.replace('/', "-"))
}

/// Resolve the absolute path to the JSONL transcript for `session_id` rooted
/// at `cwd`. Honors the `ARK_CLAUDE_PROJECTS_DIR` env override.
pub fn transcript_path(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let projects_root = match std::env::var_os("ARK_CLAUDE_PROJECTS_DIR") {
        Some(v) => PathBuf::from(v),
        None => {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            home.join(".claude").join("projects")
        }
    };
    let encoded = encode_cwd(cwd)?;
    Some(
        projects_root
            .join(encoded)
            .join(format!("{session_id}.jsonl")),
    )
}

/// Truncate a string to at most `max` characters at a UTF-8 char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Parse one JSONL line into zero or more [`AgentEvent`] values.
///
/// Returns an empty vec for malformed JSON, ignored block kinds
/// (`tool_result`, `thinking`), or shapes the parser does not recognize. The
/// caller logs and continues.
pub fn parse_line(line: &str, id: &AgentId) -> Vec<AgentEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, line = %trimmed, "transcript: malformed JSON line");
            return Vec::new();
        }
    };

    let kind = block_kind(&value);
    match kind.as_deref() {
        Some("user") | Some("user_message") => {
            let summary = extract_message_text(&value);
            vec![AgentEvent::Message {
                id: id.clone(),
                role: MessageRole::User,
                summary: truncate_chars(&summary, SUMMARY_MAX_CHARS),
            }]
        }
        Some("assistant") | Some("assistant_message") => {
            let mut out = Vec::new();
            // Assistant messages can carry tool_use blocks inside content.
            for tu in extract_tool_uses(&value) {
                out.extend(events_for_tool_use(&tu, id));
            }
            let text = extract_message_text(&value);
            if !text.is_empty() {
                out.push(AgentEvent::Message {
                    id: id.clone(),
                    role: MessageRole::Assistant,
                    summary: truncate_chars(&text, SUMMARY_MAX_CHARS),
                });
            }
            out
        }
        Some("tool_use") => events_for_tool_use(&value, id),
        Some("tool_result") | Some("thinking") => {
            tracing::debug!(kind = ?kind, "transcript: skipped block");
            Vec::new()
        }
        _ => {
            tracing::debug!(value = %trimmed, "transcript: unknown block shape");
            Vec::new()
        }
    }
}

/// Extract the block discriminator. Claude transcripts use both top-level
/// `type` (`"user"`, `"assistant"`, `"tool_use"`, …) and message-API style
/// nested blocks. We accept either.
fn block_kind(v: &Value) -> Option<String> {
    if let Some(t) = v.get("type").and_then(Value::as_str) {
        return Some(t.to_string());
    }
    if let Some(role) = v.get("role").and_then(Value::as_str) {
        return Some(role.to_string());
    }
    None
}

/// Best-effort text extraction. Handles:
/// - `{"message": {"content": "..."}}`
/// - `{"message": {"content": [{"type": "text", "text": "..."}, ...]}}`
/// - `{"content": "..."}` / `{"content": [...]}`
/// - `{"text": "..."}`
fn extract_message_text(v: &Value) -> String {
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"))
        .or_else(|| v.get("text"));
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut out = String::new();
            for block in arr {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Walk an assistant-message body and collect any `tool_use` content blocks.
fn extract_tool_uses(v: &Value) -> Vec<Value> {
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"));
    let Some(Value::Array(arr)) = content else {
        return Vec::new();
    };
    arr.iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .cloned()
        .collect()
}

/// Build the event(s) for a single `tool_use` block: always a `ToolUse`,
/// plus a `FileEdited` when the tool is one of Edit/Write/MultiEdit/NotebookEdit
/// and the input carries a `file_path`.
fn events_for_tool_use(tu: &Value, id: &AgentId) -> Vec<AgentEvent> {
    let tool = tu
        .get("name")
        .or_else(|| tu.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = tu.get("input").cloned().unwrap_or(Value::Null);
    let input_summary = match &input {
        Value::String(s) => truncate_chars(s, SUMMARY_MAX_CHARS),
        Value::Null => String::new(),
        other => truncate_chars(&other.to_string(), SUMMARY_MAX_CHARS),
    };
    let mut out = vec![AgentEvent::ToolUse {
        id: id.clone(),
        tool: tool.clone(),
        input_summary,
    }];
    if matches!(
        tool.as_str(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
    ) {
        if let Some(path) = input.get("file_path").and_then(Value::as_str) {
            out.push(AgentEvent::FileEdited {
                id: id.clone(),
                path: PathBuf::from(path),
                additions: 0,
                deletions: 0,
            });
        }
    }
    out
}

/// Tail Claude's session transcript, emitting [`AgentEvent`] values onto `tx`.
///
/// See the module-level docs for lifecycle semantics. `config_gate` mirrors
/// `config.engine.claude_code.transcript_tail`; when false this returns
/// immediately without touching disk.
pub async fn tail_transcript(
    cwd: PathBuf,
    session_id: String,
    config_gate: bool,
    tx: EventSink,
    id: AgentId,
    cancel: CancellationToken,
) -> Result<()> {
    if !config_gate {
        tracing::debug!("transcript_tail disabled — returning immediately");
        return Ok(());
    }

    let Some(path) = transcript_path(&cwd, &session_id) else {
        tracing::warn!(?cwd, %session_id, "transcript: cannot resolve path (non-utf8 cwd or no HOME)");
        return Ok(());
    };

    tail_transcript_path(path, tx, id, cancel).await
}

/// Like [`tail_transcript`] but takes the resolved JSONL path directly. Used
/// by tests to avoid races on the shared `ARK_CLAUDE_PROJECTS_DIR` env var.
pub async fn tail_transcript_path(
    path: PathBuf,
    tx: EventSink,
    id: AgentId,
    cancel: CancellationToken,
) -> Result<()> {
    // Wait for the file to appear (Claude creates it on first write).
    let mut tries = 0;
    while !path.exists() {
        if cancel.is_cancelled() {
            return Ok(());
        }
        tries += 1;
        if tries > FILE_NOT_FOUND_RETRIES {
            tracing::warn!(?path, "transcript: file never appeared, giving up");
            return Ok(());
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(FILE_NOT_FOUND_BACKOFF) => {}
        }
    }

    run_tail(&path, &tx, &id, &cancel).await
}

async fn run_tail(
    path: &Path,
    tx: &EventSink,
    id: &AgentId,
    cancel: &CancellationToken,
) -> Result<()> {
    // Notify watcher → channel, polled cooperatively below.
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<()>();
    let notify_tx_cb = notify_tx.clone();
    let watcher_res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = notify_tx_cb.send(());
        }
    });
    let mut watcher = match watcher_res {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "transcript: failed to create notify watcher");
            return Ok(());
        }
    };
    if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
        tracing::warn!(error = %e, ?path, "transcript: failed to watch file");
        return Ok(());
    }

    let file = File::open(path)
        .await
        .with_context(|| format!("opening transcript at {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut last_pos: u64 = 0;

    loop {
        // Drain available lines.
        loop {
            buf.clear();
            let n = match reader.read_line(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "transcript: read error");
                    return Ok(());
                }
            };
            if n == 0 {
                break;
            }
            // Only dispatch full lines (terminated with \n). Partial trailing
            // lines stay in the file; we'll re-read them once they're flushed.
            if !buf.ends_with('\n') {
                // Roll back to before the partial line so the next read picks
                // it up complete.
                let partial_len = buf.len() as u64;
                if let Err(e) = reader.seek(SeekFrom::Current(-(partial_len as i64))).await {
                    tracing::debug!(error = %e, "transcript: seek-back failed");
                }
                break;
            }
            last_pos += n as u64;
            for ev in parse_line(&buf, id) {
                let _ = tx.send(ev);
            }
        }

        // Wait for the next signal: cancel, file change, or a small timeout
        // (defensive — covers fs implementations that drop change events).
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            maybe = notify_rx.recv() => {
                if maybe.is_none() {
                    // Watcher channel closed unexpectedly — bail.
                    return Ok(());
                }
                // Detect rotation/truncation: if the file shrunk below our
                // current cursor, seek to start and re-read.
                if let Ok(meta) = tokio::fs::metadata(path).await {
                    if meta.len() < last_pos {
                        tracing::debug!("transcript: detected truncation, restarting from byte 0");
                        if reader.seek(SeekFrom::Start(0)).await.is_ok() {
                            last_pos = 0;
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, channel};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "test")
    }

    #[test]
    fn truncate_at_char_boundary_ascii() {
        let s = "x".repeat(100);
        let t = truncate_chars(&s, 80);
        assert_eq!(t.chars().count(), 80);
    }

    #[test]
    fn truncate_at_char_boundary_multibyte() {
        // 100 4-byte emoji → byte-slicing at 80 would split a codepoint.
        let s: String = "🦀".repeat(100);
        let t = truncate_chars(&s, 80);
        assert_eq!(t.chars().count(), 80);
        // Round-trip is valid UTF-8 (would have panicked otherwise).
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn parse_user_message_emits_message_user() {
        let id = sample_id();
        let line = r#"{"type":"user","message":{"role":"user","content":"hello world"}}"#;
        let evs = parse_line(line, &id);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::Message { role, summary, .. } => {
                assert_eq!(*role, MessageRole::User);
                assert_eq!(summary, "hello world");
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_message_emits_message_assistant() {
        let id = sample_id();
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"sure"}]}}"#;
        let evs = parse_line(line, &id);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::Message { role, summary, .. } => {
                assert_eq!(*role, MessageRole::Assistant);
                assert_eq!(summary, "sure");
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_use_edit_emits_tool_use_and_file_edited() {
        let id = sample_id();
        let line = r#"{"type":"tool_use","name":"Edit","input":{"file_path":"/tmp/foo.rs","old_string":"a","new_string":"b"}}"#;
        let evs = parse_line(line, &id);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::ToolUse { tool, .. } => assert_eq!(tool, "Edit"),
            other => panic!("expected ToolUse, got {other:?}"),
        }
        match &evs[1] {
            AgentEvent::FileEdited { path, .. } => {
                assert_eq!(path, &PathBuf::from("/tmp/foo.rs"));
            }
            other => panic!("expected FileEdited, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_use_read_no_file_edited() {
        let id = sample_id();
        let line = r#"{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/foo.rs"}}"#;
        let evs = parse_line(line, &id);
        assert_eq!(evs.len(), 1);
        assert!(matches!(evs[0], AgentEvent::ToolUse { .. }));
    }

    #[test]
    fn parse_tool_result_ignored() {
        let id = sample_id();
        let line = r#"{"type":"tool_result","tool_use_id":"abc","content":"ok"}"#;
        assert!(parse_line(line, &id).is_empty());
    }

    #[test]
    fn parse_thinking_ignored() {
        let id = sample_id();
        let line = r#"{"type":"thinking","thinking":"hmm"}"#;
        assert!(parse_line(line, &id).is_empty());
    }

    #[test]
    fn parse_malformed_returns_empty() {
        let id = sample_id();
        assert!(parse_line("{not json", &id).is_empty());
        assert!(parse_line("", &id).is_empty());
    }

    #[test]
    fn assistant_message_with_tool_use_emits_both() {
        let id = sample_id();
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/x.txt","content":"hi"}},{"type":"text","text":"done"}]}}"#;
        let evs = parse_line(line, &id);
        // ToolUse + FileEdited + Message
        assert_eq!(evs.len(), 3);
        assert!(matches!(evs[0], AgentEvent::ToolUse { .. }));
        assert!(matches!(evs[1], AgentEvent::FileEdited { .. }));
        assert!(matches!(evs[2], AgentEvent::Message { .. }));
    }

    #[test]
    fn encode_cwd_replaces_slashes() {
        assert_eq!(
            encode_cwd(Path::new("/Users/rjm/foo")).as_deref(),
            Some("-Users-rjm-foo")
        );
    }

    // Single-process mutex around tests that touch the shared
    // `ARK_CLAUDE_PROJECTS_DIR` env var. cargo test runs tests in parallel
    // threads of one process; `set_var` is process-global and unsynchronized.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn transcript_path_honors_env_override() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        // SAFETY: serialized via ENV_GUARD; restored before unlock.
        unsafe {
            std::env::set_var("ARK_CLAUDE_PROJECTS_DIR", tmp.path());
        }
        let p = transcript_path(Path::new("/a/b"), "sid").unwrap();
        assert_eq!(p, tmp.path().join("-a-b").join("sid.jsonl"));
        unsafe {
            std::env::remove_var("ARK_CLAUDE_PROJECTS_DIR");
        }
    }

    #[tokio::test]
    async fn config_gate_false_returns_immediately() {
        let (tx, _rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        // cwd / session_id chosen to NOT exist on disk — proves no IO touch.
        let cwd = PathBuf::from("/definitely/does/not/exist/ark-test");
        tail_transcript(cwd, "nope".into(), false, tx, id, cancel)
            .await
            .expect("noop returns Ok");
    }

    #[tokio::test]
    async fn file_not_found_retries_then_returns_ok() {
        // Uses tail_transcript_path with a path inside a tmp dir that's never
        // created — exercises the retry-then-give-up path without env races.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nofile.jsonl");
        let (tx, _rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            cancel2.cancel();
        });
        tail_transcript_path(path, tx, id, cancel)
            .await
            .expect("returns Ok on missing file");
    }

    // -----------------------------------------------------------------
    // T-120: partial-line / chunk-split coverage.
    //
    // The tailer handles the case where a writer flushes half a JSONL
    // record (no trailing newline) by seeking back and waiting for the
    // rest. Pin that behavior so a future rewrite of `run_tail`'s read
    // loop can't silently reintroduce dropped-partial-line bugs.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn partial_line_is_buffered_until_newline_arrives() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("session.jsonl");
        tokio::fs::write(&path, b"").await.unwrap();

        let (tx, mut rx) = channel(64);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let path_clone = path.clone();
        let handle =
            tokio::spawn(async move { tail_transcript_path(path_clone, tx, id, cancel2).await });

        // Write the first half of a JSON line WITHOUT a terminating
        // newline. The tailer must NOT parse-and-dispatch this yet.
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        let head = r#"{"type":"user","message":{"role":"user","content":"spl"#;
        f.write_all(head.as_bytes()).await.unwrap();
        f.flush().await.unwrap();

        // Give the watcher a chance to wake and try to read.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // No event should have been emitted from the partial line.
        let maybe = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            maybe.is_err() || (maybe.is_ok() && maybe.unwrap().is_err()),
            "partial line must not emit events before newline arrives"
        );

        // Finish the line and add a newline — the watcher should now
        // parse the complete record and emit one Message event.
        let tail = "it-across-chunks\"}}\n";
        f.write_all(tail.as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut got: Option<AgentEvent> = None;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                got = Some(ev);
                break;
            }
        }
        let ev = got.expect("expected Message event after line completes");
        match ev {
            AgentEvent::Message { role, summary, .. } => {
                assert_eq!(role, MessageRole::User);
                assert_eq!(summary, "split-across-chunks");
            }
            other => panic!("expected Message, got {other:?}"),
        }

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn line_split_across_three_writes_parses_once() {
        // Emulate a slow writer that flushes a single record in three
        // separate chunks, the first two without a newline. Exactly one
        // event must result.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("session.jsonl");
        tokio::fs::write(&path, b"").await.unwrap();

        let (tx, mut rx) = channel(64);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let path_clone = path.clone();
        let handle =
            tokio::spawn(async move { tail_transcript_path(path_clone, tx, id, cancel2).await });

        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();

        let chunks: [&[u8]; 3] = [
            br#"{"type":"tool_use","name":"#,
            br#""Read","input":{"file_path":"#,
            b"\"/tmp/foo.rs\"}}\n",
        ];
        for c in chunks {
            f.write_all(c).await.unwrap();
            f.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        drop(f);

        // Collect events for up to 5s; expect exactly one ToolUse once
        // all chunks land.
        let mut evs = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
                Ok(Ok(ev)) => evs.push(ev),
                _ => {
                    if !evs.is_empty() {
                        break;
                    }
                }
            }
        }
        assert_eq!(
            evs.len(),
            1,
            "expected exactly one ToolUse once all chunks land, got {evs:?}"
        );
        match &evs[0] {
            AgentEvent::ToolUse { tool, .. } => assert_eq!(tool, "Read"),
            other => panic!("expected ToolUse, got {other:?}"),
        }

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn append_path_emits_initial_then_appended() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("session.jsonl");

        // Pre-populate two lines.
        let initial = r#"{"type":"user","message":{"role":"user","content":"line1"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"line2"}]}}
"#;
        tokio::fs::write(&path, initial).await.unwrap();

        let (tx, mut rx) = channel(64);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let path_clone = path.clone();
        let handle =
            tokio::spawn(async move { tail_transcript_path(path_clone, tx, id, cancel2).await });

        // Receive initial two events.
        let mut got = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while got.len() < 2 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                got.push(ev);
            }
        }
        assert_eq!(got.len(), 2, "initial replay");

        // Append two more lines.
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        let more = r#"{"type":"tool_use","name":"Read","input":{"file_path":"/x"}}
{"type":"user","message":{"role":"user","content":"line4"}}
"#;
        f.write_all(more.as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        let mut got2 = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while got2.len() < 2 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                got2.push(ev);
            }
        }
        assert_eq!(got2.len(), 2, "appended events");

        cancel.cancel();
        let _ = handle.await;
    }
}
