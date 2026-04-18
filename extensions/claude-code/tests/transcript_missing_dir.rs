//! T-028 integration — transcript-dir missing at session start.
//!
//! Acceptance criteria (build-site-claude-code-ext.md T-028):
//!
//! 1. Temp dir with NO transcript file at the watched parent.
//! 2. `TranscriptDirWatcher::ensure_tracking(missing_dir)` does NOT
//!    error and logs `awaiting transcript dir` at INFO.
//! 3. The file-system-level transcript dir is created afterwards
//!    (simulating Claude Code creating `~/.claude/projects/<encoded>/`
//!    on its first session event).
//! 4. A second `ensure_tracking` call (mimicking the accept-loop's
//!    per-frame re-invocation) binds the watch.
//! 5. A transcript-file write inside the now-existing dir produces a
//!    `TranscriptDirEvent` within a short polling deadline.
//!
//! Uses a thread-scoped `tracing_subscriber::Layer` (same pattern as
//! `ark-ext-test-support/tests/version_mismatch.rs`) so the INFO log
//! is captured without touching a global subscriber. Poll-with-timeout
//! rather than assert-immediate because macOS FSEvents has variable
//! latency (per constraint in the task brief).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_ext_claude_code::{TranscriptDirEvent, TranscriptDirWatcher};
use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;

// ---------------------------------------------------------------------------
// Thread-scoped tracing capture
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct CapturedEvent {
    level: String,
    message: Option<String>,
    dir: Option<String>,
}

struct CaptureVisitor<'a> {
    out: &'a mut CapturedEvent,
}

impl<'a> Visit for CaptureVisitor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.out.message = Some(value.to_string()),
            "dir" => self.out.dir = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let stripped = rendered.trim_matches('"').to_string();
        match field.name() {
            "message" => {
                if self.out.message.is_none() {
                    self.out.message = Some(stripped);
                }
            }
            "dir" => {
                if self.out.dir.is_none() {
                    self.out.dir = Some(stripped);
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        true
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut captured = CapturedEvent {
            level: event.metadata().level().to_string(),
            ..Default::default()
        };
        let mut visitor = CaptureVisitor { out: &mut captured };
        event.record(&mut visitor);
        self.events.lock().unwrap().push(captured);
    }
}

fn with_capture<R, F>(f: F) -> (R, Vec<CapturedEvent>)
where
    F: FnOnce() -> R,
{
    let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CaptureLayer {
        events: events.clone(),
    };
    let subscriber = Registry::default().with(layer);
    tracing::callsite::rebuild_interest_cache();
    let result = tracing::subscriber::with_default(subscriber, f);
    let events_snapshot = events.lock().unwrap().clone();
    (result, events_snapshot)
}

// ---------------------------------------------------------------------------
// T-028 regression
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn watcher_waits_and_reacts_when_transcript_dir_appears() {
    let tmp = TempDir::new().unwrap();
    // The dir Claude Code will create on first session event. We
    // intentionally do NOT mkdir it here — the point of T-028 is to
    // drive the missing-then-created sequence.
    let missing_dir: PathBuf = tmp.path().join("projects").join("-Users-rjm-repo");

    // --- Phase 1: missing dir → log INFO, no active watch. ---
    let ((mut watcher, mut rx), logs) = with_capture(|| {
        let (mut w, rx) = TranscriptDirWatcher::new();
        w.ensure_tracking(&missing_dir);
        (w, rx)
    });

    assert!(
        watcher.tracked_dir().is_none(),
        "missing dir must leave tracked_dir None"
    );
    // At least one INFO with the expected sentinel message must have
    // been captured.
    let found_waiting = logs.iter().any(|e| {
        e.level == "INFO"
            && e.message
                .as_deref()
                .map(|m| m.contains("awaiting transcript dir"))
                .unwrap_or(false)
    });
    assert!(
        found_waiting,
        "expected INFO log containing `awaiting transcript dir`; got {logs:?}"
    );
    // And NO ERROR logs — T-028 is graceful.
    assert!(
        !logs.iter().any(|e| e.level == "ERROR"),
        "missing dir must not error: {logs:?}"
    );

    // --- Phase 2: Claude Code creates the dir. ---
    std::fs::create_dir_all(&missing_dir).unwrap();

    // --- Phase 3: accept-loop re-invokes ensure_tracking. ---
    watcher.ensure_tracking(&missing_dir);
    assert_eq!(
        watcher.tracked_dir(),
        Some(missing_dir.as_path()),
        "watch must bind after dir appears"
    );

    // --- Phase 4: a transcript write lands an event. ---
    // Give notify a moment to register on macOS FSEvents.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let tx_path = missing_dir.join("session.jsonl");
    std::fs::write(&tx_path, b"{\"role\":\"user\"}\n").unwrap();

    // Poll-with-timeout per the macOS FSEvents constraint — we only
    // assert *some* event arrives, not the exact variant ordering.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
            Ok(Some(ev)) => match ev {
                TranscriptDirEvent::SubagentTranscriptCreated(_)
                | TranscriptDirEvent::TranscriptModified(_) => {
                    saw = true;
                    break;
                }
            },
            Ok(None) => break,
            Err(_) => continue, // timeout; keep draining
        }
    }
    assert!(
        saw,
        "watcher failed to react to transcript write after late-bind within 3s"
    );
}
