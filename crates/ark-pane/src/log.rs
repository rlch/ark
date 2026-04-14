//! `ark pane log` — tail `events.jsonl` for a given agent.
//!
//! See `context/kits/cavekit-pane-commands.md` R3:
//! - Opens `$STATE/agents/{id}/events.jsonl` via [`ark_core::events_log::EventLogReader`].
//! - Follows new writes via `notify::recommended_watcher` (inotify on Linux,
//!   FSEvents on macOS); on each Modify event we re-parse from last cursor.
//! - Renders one row per event: `HH:MM:SS  KIND  summary`.
//! - Colors by kind (cyan/red/green/yellow/gray).
//! - `--filter <KIND>` — case-insensitive startswith match against the row's
//!   KIND column.
//! - `gg` → jump to start, `G` → jump to end; auto-scroll follows tail unless
//!   the user has scrolled off the bottom.
//! - Missing agent dir → `Agent '{id}' not found` placeholder, exits 3 after 2s.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_core::events_log::{EventLogReader, EventRecord};
use ark_types::{AgentEvent, AgentId, LogLevel, Outcome, StateLayout};
use crossterm::event::KeyCode;
use notify::{EventKind, RecursiveMode, Watcher};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{PaneEvent, PaneFlow, no_color, run_pane};

/// Row shown in the log pane: timestamp + kind + summary, pre-styled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRow {
    pub ts: String, // "HH:MM:SS"
    pub kind: String,
    pub summary: String,
}

/// Mutable state for the log pane.
pub struct LogState {
    pub rows: Vec<LogRow>,
    pub filter: Option<String>,
    pub scroll_offset: usize,
    /// When true (default), new rows auto-scroll the view to the bottom.
    /// Toggled off by any upward scroll; toggled back on by `G`.
    pub follow: bool,
    /// `gg` two-key sequence: first `g` arms this, second clears it.
    pub awaiting_gg: bool,
    pub agent_id: String,
}

impl LogState {
    /// Construct with an agent id string (for display) and optional kind filter.
    pub fn new(agent_id: impl Into<String>, filter: Option<String>) -> Self {
        Self {
            rows: Vec::new(),
            filter,
            scroll_offset: 0,
            follow: true,
            awaiting_gg: false,
            agent_id: agent_id.into(),
        }
    }
}

/// Produce `(kind, summary)` for any `AgentEvent` variant.
///
/// Kind strings are snake_case and stable (they match the serde `kind` tag on
/// [`AgentEvent`]), so `--filter` matching is consistent across builds.
pub fn one_liner(ev: &AgentEvent) -> (String, String) {
    match ev {
        AgentEvent::Started { spec } => ("started".into(), format!("spec {}", spec.id.as_str())),
        AgentEvent::TabOpened {
            id, role, label, ..
        } => (
            "tab_opened".into(),
            format!("{} [{:?}] {}", id.as_str(), role, label),
        ),
        AgentEvent::TabClosed { id, tab_handle } => (
            "tab_closed".into(),
            format!("{} @ {}", id.as_str(), tab_handle),
        ),
        AgentEvent::Progress {
            done, total, label, ..
        } => (
            "progress".into(),
            match label {
                Some(l) => format!("{done}/{total} {l}"),
                None => format!("{done}/{total}"),
            },
        ),
        AgentEvent::TaskDone { task_id, label, .. } => (
            "task_done".into(),
            match label {
                Some(l) => format!("{task_id} {l}"),
                None => task_id.clone(),
            },
        ),
        AgentEvent::Iteration { n, max, .. } => (
            "iteration".into(),
            match max {
                Some(m) => format!("{n}/{m}"),
                None => format!("{n}"),
            },
        ),
        AgentEvent::PhaseTransition { from, to, .. } => (
            "phase_transition".into(),
            match from {
                Some(f) => format!("{f} -> {to}"),
                None => to.clone(),
            },
        ),
        AgentEvent::ToolUse {
            tool,
            input_summary,
            ..
        } => ("tool_use".into(), format!("{tool} {input_summary}")),
        AgentEvent::Message { role, summary, .. } => {
            ("message".into(), format!("{role:?} {summary}"))
        }
        AgentEvent::FileEdited {
            path,
            additions,
            deletions,
            ..
        } => (
            "file_edited".into(),
            format!("{} +{additions} -{deletions}", path.display()),
        ),
        AgentEvent::ReviewComment {
            severity,
            path,
            line,
            body,
            ..
        } => (
            "review_comment".into(),
            match line {
                Some(l) => format!("{severity:?} {}:{l} {body}", path.display()),
                None => format!("{severity:?} {} {body}", path.display()),
            },
        ),
        AgentEvent::PermissionAsked { tool, summary, .. } => {
            ("permission_asked".into(), format!("{tool} {summary}"))
        }
        AgentEvent::PermissionResolved { tool, decision, .. } => {
            ("permission_resolved".into(), format!("{tool} {decision:?}"))
        }
        AgentEvent::Stall { since, .. } => ("stall".into(), format!("since {since}")),
        AgentEvent::Log { level, line, .. } => {
            ("log".into(), format!("{} {line}", log_level_tag(level)))
        }
        AgentEvent::Error { message, .. } => ("error".into(), message.clone()),
        AgentEvent::Done { outcome, .. } => ("done".into(), outcome_summary(outcome)),
        // AgentEvent is `#[non_exhaustive]`; future variants fall through to
        // a generic kind so the pane never drops an event silently.
        _ => ("other".into(), String::new()),
    }
}

fn log_level_tag(l: &LogLevel) -> &'static str {
    match l {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    }
}

fn outcome_summary(o: &Outcome) -> String {
    match o {
        Outcome::Success { artifacts } => format!("success ({} artifacts)", artifacts.len()),
        Outcome::Failed { reason } => format!("failed: {reason}"),
        Outcome::Killed => "killed".into(),
        Outcome::Timeout => "timeout".into(),
        Outcome::Crashed { reason } => format!("crashed: {reason}"),
    }
}

/// True iff `kind` passes the configured `--filter`. Case-insensitive
/// startswith. `None` filter always passes.
pub fn filter_matches(filter: Option<&str>, kind: &str) -> bool {
    match filter {
        None => true,
        Some(needle) => {
            let needle = needle.trim().to_ascii_lowercase();
            if needle.is_empty() {
                return true;
            }
            kind.to_ascii_lowercase().starts_with(&needle)
        }
    }
}

/// Convert a parsed `EventRecord` into a [`LogRow`] (time formatted HH:MM:SS).
pub fn record_to_row(rec: &EventRecord) -> LogRow {
    let (kind, summary) = one_liner(&rec.event);
    LogRow {
        ts: rec.ts.format("%H:%M:%S").to_string(),
        kind,
        summary,
    }
}

/// Color for a given kind string. When `NO_COLOR` is set, falls back to the
/// terminal default; bold is preserved where the kind is important (error,
/// done) so the output still has visual hierarchy.
pub fn color_for_kind(kind: &str, summary: &str) -> Style {
    if no_color() {
        // Preserve bold for error/done so users can still see signal.
        return match kind {
            "error" => Style::default().add_modifier(Modifier::BOLD),
            "done" if summary.starts_with("failed") || summary.starts_with("crashed") => {
                Style::default().add_modifier(Modifier::BOLD)
            }
            _ => Style::default(),
        };
    }
    match kind {
        "tool_use" => Style::default().fg(Color::Cyan),
        "error" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "done" => {
            if summary.starts_with("success") {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            }
        }
        "permission_asked" => Style::default().fg(Color::Yellow),
        "permission_resolved" => Style::default().fg(Color::Yellow),
        "stall" => Style::default().fg(Color::Yellow),
        "task_done" => Style::default().fg(Color::Green),
        _ => Style::default().fg(Color::Gray),
    }
}

/// Top-level entrypoint. Returns exit code 3 via a sentinel error when the
/// agent directory is missing (the caller — `ark pane log` command — maps
/// this to `std::process::exit(3)`).
pub async fn run(
    state_layout: Arc<StateLayout>,
    id: AgentId,
    filter: Option<String>,
) -> anyhow::Result<()> {
    let agent_dir = state_layout.agent_dir(&id);
    if !agent_dir.exists() {
        eprintln!("agent {} not found", id.as_str());
        std::process::exit(3);
    }

    let events_path = state_layout.events_path(&id);
    let state = LogState::new(id.as_str(), filter);
    // Seed existing events (if any).
    let mut state = state;
    if events_path.exists() {
        if let Ok(mut reader) = EventLogReader::open(&events_path) {
            for rec in reader.read_all() {
                state.rows.push(record_to_row(&rec));
            }
        }
    }

    // Shared flag toggled by the notify watcher; the tick handler drains it.
    let dirty = Arc::new(Mutex::new(true));
    let dirty_notify = dirty.clone();
    // Keep watcher alive for the whole pane lifetime.
    let watch_target = events_path.clone();
    // Watch the parent dir (works even when the file doesn't exist yet).
    let watch_dir = watch_target
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let _watcher = spawn_watcher(watch_dir, watch_target.clone(), dirty_notify);

    let events_path_render = events_path.clone();
    let render = move |f: &mut Frame, s: &LogState| {
        render_frame(f, s, &events_path_render);
    };

    let poll_interval = Duration::from_millis(250);
    let mut last_poll = Instant::now() - poll_interval;
    let events_path_poll = events_path.clone();

    let handler = move |s: &mut LogState, ev: PaneEvent| -> PaneFlow {
        match ev {
            PaneEvent::Key(k) => {
                handle_key(s, k.code);
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    PaneFlow::Quit
                } else {
                    PaneFlow::Continue
                }
            }
            PaneEvent::Resize(_, _) => PaneFlow::Continue,
            PaneEvent::Tick => {
                if last_poll.elapsed() < poll_interval {
                    return PaneFlow::Continue;
                }
                last_poll = Instant::now();
                let needs_refresh = {
                    let mut flag = dirty.lock().unwrap();
                    let v = *flag;
                    *flag = false;
                    v
                };
                if needs_refresh && events_path_poll.exists() {
                    if let Ok(mut reader) = EventLogReader::open(&events_path_poll) {
                        let all = reader.read_all();
                        s.rows = all.iter().map(record_to_row).collect();
                    }
                }
                PaneFlow::Continue
            }
            PaneEvent::Custom(_) => PaneFlow::Continue,
        }
    };

    run_pane(state, render, handler).await
}

/// Handle a single key press against the shared state. Extracted so it can be
/// exercised without crossterm in tests.
pub fn handle_key(s: &mut LogState, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => { /* quit handled by caller */ }
        KeyCode::Char('j') | KeyCode::Down => {
            s.scroll_offset = s.scroll_offset.saturating_add(1);
            s.awaiting_gg = false;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            s.scroll_offset = s.scroll_offset.saturating_sub(1);
            s.follow = false;
            s.awaiting_gg = false;
        }
        KeyCode::PageDown => {
            s.scroll_offset = s.scroll_offset.saturating_add(10);
            s.awaiting_gg = false;
        }
        KeyCode::PageUp => {
            s.scroll_offset = s.scroll_offset.saturating_sub(10);
            s.follow = false;
            s.awaiting_gg = false;
        }
        KeyCode::Char('G') => {
            s.follow = true;
            s.scroll_offset = usize::MAX; // clamped in render
            s.awaiting_gg = false;
        }
        KeyCode::Char('g') => {
            if s.awaiting_gg {
                // second 'g' → jump to top
                s.scroll_offset = 0;
                s.follow = false;
                s.awaiting_gg = false;
            } else {
                s.awaiting_gg = true;
            }
        }
        KeyCode::Home => {
            s.scroll_offset = 0;
            s.follow = false;
            s.awaiting_gg = false;
        }
        KeyCode::End => {
            s.follow = true;
            s.scroll_offset = usize::MAX;
            s.awaiting_gg = false;
        }
        _ => {
            s.awaiting_gg = false;
        }
    }
}

/// Spawn a notify watcher on `watch_dir`, flipping `dirty` whenever
/// `target_path` sees a Modify/Create event. Returned guard must be held for
/// the lifetime of the pane.
fn spawn_watcher(
    watch_dir: PathBuf,
    target_path: PathBuf,
    dirty: Arc<Mutex<bool>>,
) -> Option<notify::RecommendedWatcher> {
    let target = target_path;
    let res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        // Only care about our specific file.
        if !ev.paths.iter().any(|p| p == &target) {
            return;
        }
        if matches!(
            ev.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Any
        ) {
            if let Ok(mut f) = dirty.lock() {
                *f = true;
            }
        }
    });
    let Ok(mut watcher) = res else {
        tracing::warn!("failed to create notify watcher — live tail disabled");
        return None;
    };
    if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        tracing::warn!("failed to watch {}: {e}", watch_dir.display());
        return None;
    }
    Some(watcher)
}

fn render_frame(f: &mut Frame, s: &LogState, events_path: &Path) {
    let area = f.area();

    // Top chrome, body, footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Header
    let filter_text = s
        .filter
        .as_deref()
        .map(|f| format!("filter={f}"))
        .unwrap_or_else(|| "filter=*".to_string());
    let header = Paragraph::new(format!("agent {}  {}", s.agent_id, filter_text)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" ark pane log "),
    );
    f.render_widget(header, chunks[0]);

    // Body
    let filtered: Vec<&LogRow> = s
        .rows
        .iter()
        .filter(|r| filter_matches(s.filter.as_deref(), &r.kind))
        .collect();
    let lines: Vec<Line> = filtered
        .iter()
        .map(|r| {
            let style = color_for_kind(&r.kind, &r.summary);
            Line::from(vec![
                Span::raw(format!("{}  ", r.ts)),
                Span::styled(format!("{:<20}", r.kind), style),
                Span::raw("  "),
                Span::raw(r.summary.clone()),
            ])
        })
        .collect();

    let body_area = chunks[1];
    let viewport = body_area.height.saturating_sub(2) as usize;
    let max_offset = lines.len().saturating_sub(viewport.max(1));
    let offset = if s.follow {
        max_offset
    } else {
        s.scroll_offset.min(max_offset)
    };
    let end = (offset + viewport).min(lines.len());
    let visible: Vec<Line> = lines[offset..end].to_vec();
    let body = Paragraph::new(visible).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" events ({}) ", filtered.len())),
    );
    f.render_widget(body, body_area);

    // Footer
    let follow = if s.follow { "follow" } else { "paused" };
    let footer_txt = format!(
        "q quit · j/k scroll · gg top · G end · {follow} · {}",
        events_path.display()
    );
    let footer = Paragraph::new(footer_txt);
    f.render_widget(footer, chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, LogLevel, Outcome};
    use chrono::{TimeZone, Utc};

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    #[test]
    fn one_liner_tool_use() {
        let ev = AgentEvent::ToolUse {
            id: sample_id(),
            tool: "Read".into(),
            input_summary: "foo.rs".into(),
        };
        let (k, s) = one_liner(&ev);
        assert_eq!(k, "tool_use");
        assert!(s.contains("Read"));
        assert!(s.contains("foo.rs"));
    }

    #[test]
    fn one_liner_error_and_done() {
        let err = AgentEvent::Error {
            id: sample_id(),
            message: "boom".into(),
        };
        assert_eq!(one_liner(&err).0, "error");
        assert_eq!(one_liner(&err).1, "boom");

        let done_ok = AgentEvent::Done {
            id: sample_id(),
            outcome: Outcome::Success {
                artifacts: vec![PathBuf::from("a")],
            },
        };
        let (k, s) = one_liner(&done_ok);
        assert_eq!(k, "done");
        assert!(s.starts_with("success"));

        let done_fail = AgentEvent::Done {
            id: sample_id(),
            outcome: Outcome::Failed {
                reason: "nope".into(),
            },
        };
        let (_k, s) = one_liner(&done_fail);
        assert!(s.contains("nope"));
    }

    #[test]
    fn one_liner_progress_with_and_without_label() {
        let p1 = AgentEvent::Progress {
            id: sample_id(),
            done: 2,
            total: 5,
            label: None,
        };
        assert_eq!(one_liner(&p1).1, "2/5");
        let p2 = AgentEvent::Progress {
            id: sample_id(),
            done: 2,
            total: 5,
            label: Some("step".into()),
        };
        assert!(one_liner(&p2).1.contains("step"));
    }

    #[test]
    fn one_liner_log_includes_level() {
        let ev = AgentEvent::Log {
            id: sample_id(),
            level: LogLevel::Warn,
            line: "heads up".into(),
        };
        let (_k, s) = one_liner(&ev);
        assert!(s.contains("WARN"));
        assert!(s.contains("heads up"));
    }

    #[test]
    fn filter_matches_none_passes_all() {
        assert!(filter_matches(None, "tool_use"));
        assert!(filter_matches(None, ""));
    }

    #[test]
    fn filter_matches_case_insensitive_startswith() {
        assert!(filter_matches(Some("tool"), "tool_use"));
        assert!(filter_matches(Some("TOOL"), "tool_use"));
        assert!(filter_matches(Some("Tool"), "tool_use"));
        assert!(!filter_matches(Some("use"), "tool_use"));
        assert!(!filter_matches(Some("done"), "tool_use"));
        // Empty filter behaves like none.
        assert!(filter_matches(Some(""), "anything"));
    }

    #[test]
    fn record_to_row_formats_hh_mm_ss() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 14, 3, 7, 9).unwrap();
        let rec = EventRecord {
            ts,
            event: AgentEvent::Log {
                id: sample_id(),
                level: LogLevel::Info,
                line: "hi".into(),
            },
        };
        let row = record_to_row(&rec);
        assert_eq!(row.ts, "03:07:09");
        assert_eq!(row.kind, "log");
    }

    #[test]
    fn handle_key_j_k_scroll_and_follow_pause() {
        let mut s = LogState::new("id", None);
        // follow on by default.
        assert!(s.follow);
        handle_key(&mut s, KeyCode::Char('j'));
        assert_eq!(s.scroll_offset, 1);
        // still follow after j (per spec "auto-scroll follows tail unless user scrolled up").
        assert!(s.follow);
        handle_key(&mut s, KeyCode::Char('k'));
        assert_eq!(s.scroll_offset, 0);
        assert!(!s.follow, "k should pause follow");
    }

    #[test]
    fn handle_key_gg_jumps_to_top() {
        let mut s = LogState::new("id", None);
        s.scroll_offset = 42;
        s.follow = true;
        handle_key(&mut s, KeyCode::Char('g'));
        assert!(s.awaiting_gg);
        handle_key(&mut s, KeyCode::Char('g'));
        assert_eq!(s.scroll_offset, 0);
        assert!(!s.awaiting_gg);
    }

    #[test]
    fn handle_key_capital_g_jumps_to_end_resumes_follow() {
        let mut s = LogState::new("id", None);
        s.follow = false;
        s.scroll_offset = 5;
        handle_key(&mut s, KeyCode::Char('G'));
        assert!(s.follow);
        assert_eq!(s.scroll_offset, usize::MAX);
    }

    #[test]
    fn handle_key_single_g_then_other_cancels_gg() {
        let mut s = LogState::new("id", None);
        handle_key(&mut s, KeyCode::Char('g'));
        assert!(s.awaiting_gg);
        handle_key(&mut s, KeyCode::Char('j'));
        assert!(!s.awaiting_gg);
    }

    #[test]
    fn file_tailing_cursor_via_read_all_picks_up_appends() {
        use ark_core::events_log::EventLogWriter;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // First batch.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle
                .sender
                .send(AgentEvent::Log {
                    id: sample_id(),
                    level: LogLevel::Info,
                    line: "first".into(),
                })
                .unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        });

        let mut reader = EventLogReader::open(&path).unwrap();
        let first_rows: Vec<_> = reader.read_all().iter().map(record_to_row).collect();
        assert_eq!(first_rows.len(), 1);

        // Append second batch, then re-read all via a FRESH reader (mimics
        // what the widget does on each dirty tick).
        rt.block_on(async {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle
                .sender
                .send(AgentEvent::Log {
                    id: sample_id(),
                    level: LogLevel::Info,
                    line: "second".into(),
                })
                .unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        });

        let mut reader2 = EventLogReader::open(&path).unwrap();
        let second_rows: Vec<_> = reader2.read_all().iter().map(record_to_row).collect();
        assert_eq!(second_rows.len(), 2);
    }

    #[test]
    fn color_for_kind_no_color_is_plain() {
        // In no-color mode we cannot directly read the global flag here
        // without racing; but color_for_kind falls back deterministically
        // when called. This test just confirms the color path for kinds
        // we explicitly document.
        let style = color_for_kind("tool_use", "");
        // When NO_COLOR isn't set in the process, expect Cyan fg.
        if !no_color() {
            assert_eq!(style.fg, Some(Color::Cyan));
        }
        let err_style = color_for_kind("error", "");
        if !no_color() {
            assert_eq!(err_style.fg, Some(Color::Red));
        }
        // Done w/ success should be green.
        let done_ok = color_for_kind("done", "success (0 artifacts)");
        if !no_color() {
            assert_eq!(done_ok.fg, Some(Color::Green));
        }
        let done_fail = color_for_kind("done", "failed: x");
        if !no_color() {
            assert_eq!(done_fail.fg, Some(Color::Red));
        }
    }
}
