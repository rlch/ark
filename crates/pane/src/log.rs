//! `ark pane log` — tail `events.jsonl` for a given session.
//!
//! See `context/kits/cavekit-pane-commands.md` R3:
//! - Opens `$STATE/sessions/{id}/events.jsonl` via [`ark_core::events_log::EventLogReader`].
//! - Follows new writes via `notify::recommended_watcher` (inotify on Linux,
//!   FSEvents on macOS); on each Modify event we re-parse from last cursor.
//! - Renders one row per event: `HH:MM:SS  KIND  summary`.
//! - Colors by kind (cyan/red/green/yellow/gray).
//! - `--filter <KIND>` — case-insensitive startswith match against the row's
//!   KIND column.
//! - `gg` → jump to start, `G` → jump to end; auto-scroll follows tail unless
//!   the user has scrolled off the bottom.
//! - Missing session dir → `session '{id}' not found` placeholder, exits 3.
//!
//! Post-cavekit-soul Phase 1 the rich `AgentEvent` variants are gone; events
//! now arrive as the narrow [`CoreEvent`] enum where everything
//! methodology-flavoured rides inside `CoreEvent::Ext(ExtEvent { ext, kind,
//! payload })`. The one-liner renders the core variants as
//! `ark.core.<variant>` and extension events as `<ext>.<kind>` with a short
//! payload summary.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_core::events_log::{EventLogReader, EventRecord};
use ark_types::{CoreEvent, SessionId, StateLayout};
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
    pub follow: bool,
    /// `gg` two-key sequence: first `g` arms this, second clears it.
    pub awaiting_gg: bool,
    pub session_id: String,
}

impl LogState {
    pub fn new(session_id: impl Into<String>, filter: Option<String>) -> Self {
        Self {
            rows: Vec::new(),
            filter,
            scroll_offset: 0,
            follow: true,
            awaiting_gg: false,
            session_id: session_id.into(),
        }
    }
}

/// Compact one-line summary of a JSON payload for the summary column.
///
/// Short values (numbers, strings, bools) stringify as-is; objects and arrays
/// get truncated JSON. Empty payloads collapse to an empty string.
fn payload_summary(payload: &serde_json::Value) -> String {
    if payload.is_null() {
        return String::new();
    }
    let s = serde_json::to_string(payload).unwrap_or_default();
    if s == "null" || s == "{}" {
        return String::new();
    }
    const MAX: usize = 120;
    if s.chars().count() <= MAX {
        s
    } else {
        let mut out: String = s.chars().take(MAX.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Produce `(kind, summary)` for any [`CoreEvent`] variant.
///
/// Core variants render as `ark.core.<variant>`; extension events as
/// `<ext>.<kind>` with a short payload summary. The kind string matches
/// [`ark_types::FlatEvent::name`] for filter-column consistency.
pub fn one_liner(ev: &CoreEvent) -> (String, String) {
    match ev {
        CoreEvent::Log {
            level,
            message,
            target,
        } => {
            let t = target.as_deref().unwrap_or("");
            (
                "ark.core.log".to_string(),
                if t.is_empty() {
                    format!("{level} {message}")
                } else {
                    format!("{level} [{t}] {message}")
                },
            )
        }
        CoreEvent::Error { error } => ("ark.core.error".to_string(), error.clone()),
        CoreEvent::SessionStarted { spec } => (
            "ark.core.session_started".to_string(),
            format!("spec {}", spec.id.as_str()),
        ),
        CoreEvent::SessionEnded {
            terminated_at,
            exit: _,
        } => (
            "ark.core.session_ended".to_string(),
            format!("terminated {}", terminated_at.format("%H:%M:%S")),
        ),
        CoreEvent::Ext(ext) => {
            let name = format!("{}.{}", ext.ext, ext.kind);
            (name, payload_summary(&ext.payload))
        }
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
/// terminal default.
pub fn color_for_kind(kind: &str, _summary: &str) -> Style {
    if no_color() {
        return match kind {
            "ark.core.error" => Style::default().add_modifier(Modifier::BOLD),
            _ => Style::default(),
        };
    }
    if kind == "ark.core.error" {
        return Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    }
    if kind == "ark.core.session_started" {
        return Style::default().fg(Color::Green);
    }
    if kind == "ark.core.session_ended" {
        return Style::default().fg(Color::Yellow);
    }
    if kind == "ark.core.log" {
        return Style::default().fg(Color::Gray);
    }
    if kind.contains(".permission.") {
        return Style::default().fg(Color::Yellow);
    }
    if kind.ends_with(".tool.use") {
        return Style::default().fg(Color::Cyan);
    }
    if kind.ends_with(".task.done") {
        return Style::default().fg(Color::Green);
    }
    Style::default().fg(Color::Gray)
}

/// Top-level entrypoint. Missing session dir → stderr note + exit 3.
pub async fn run(
    state_layout: Arc<StateLayout>,
    id: SessionId,
    filter: Option<String>,
) -> anyhow::Result<()> {
    let session_dir = state_layout.session_dir(&id);
    if !session_dir.exists() {
        eprintln!("session {} not found", id.as_str());
        std::process::exit(3);
    }

    let events_path = state_layout.session_events_path(&id);
    let mut state = LogState::new(id.as_str(), filter);
    if events_path.exists() {
        if let Ok(mut reader) = EventLogReader::open(&events_path) {
            for rec in reader.read_all() {
                state.rows.push(record_to_row(&rec));
            }
        }
    }

    let dirty = Arc::new(Mutex::new(true));
    let dirty_notify = dirty.clone();
    let watch_target = events_path.clone();
    let watch_dir: PathBuf = watch_target
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

/// Handle a single key press against the shared state.
pub fn handle_key(s: &mut LogState, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => {}
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
            s.scroll_offset = usize::MAX;
            s.awaiting_gg = false;
        }
        KeyCode::Char('g') => {
            if s.awaiting_gg {
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
/// `target_path` sees a Modify/Create event.
fn spawn_watcher(
    watch_dir: PathBuf,
    target_path: PathBuf,
    dirty: Arc<Mutex<bool>>,
) -> Option<notify::RecommendedWatcher> {
    let target = target_path;
    let res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let filter_text = s
        .filter
        .as_deref()
        .map(|f| format!("filter={f}"))
        .unwrap_or_else(|| "filter=*".to_string());
    let header = Paragraph::new(format!("session {}  {}", s.session_id, filter_text)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" ark pane log "),
    );
    f.render_widget(header, chunks[0]);

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
                Span::styled(format!("{:<28}", r.kind), style),
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
    use ark_types::{CoreEvent, ExtEvent};
    use chrono::{TimeZone, Utc};

    fn ext_event(ext: &str, kind: &str, payload: serde_json::Value) -> CoreEvent {
        CoreEvent::Ext(ExtEvent {
            ext: ext.to_string(),
            kind: kind.to_string(),
            payload,
        })
    }

    #[test]
    fn one_liner_ext_tool_use() {
        let ev = ext_event(
            "claude-code",
            "tool.use",
            serde_json::json!({ "tool": "Read", "input_summary": "foo.rs" }),
        );
        let (k, s) = one_liner(&ev);
        assert_eq!(k, "claude-code.tool.use");
        assert!(s.contains("Read"));
        assert!(s.contains("foo.rs"));
    }

    #[test]
    fn one_liner_core_error() {
        let err = CoreEvent::Error {
            error: "boom".into(),
        };
        let (k, s) = one_liner(&err);
        assert_eq!(k, "ark.core.error");
        assert_eq!(s, "boom");
    }

    #[test]
    fn one_liner_core_session_ended_uses_terminated_at() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 14, 10, 30, 15).unwrap();
        let ev = CoreEvent::SessionEnded {
            terminated_at: ts,
            exit: ark_types::ExitReason::Normal,
        };
        let (k, s) = one_liner(&ev);
        assert_eq!(k, "ark.core.session_ended");
        assert!(s.contains("10:30:15"));
    }

    #[test]
    fn filter_matches_none_passes_all() {
        assert!(filter_matches(None, "tool.use"));
    }

    #[test]
    fn filter_matches_case_insensitive_startswith() {
        assert!(filter_matches(Some("claude"), "claude-code.tool.use"));
        assert!(filter_matches(Some("CLAUDE"), "claude-code.tool.use"));
        assert!(!filter_matches(Some("use"), "claude-code.tool.use"));
        assert!(filter_matches(Some(""), "anything"));
    }

    #[test]
    fn record_to_row_formats_hh_mm_ss() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 14, 3, 7, 9).unwrap();
        let rec = EventRecord {
            ts,
            event: CoreEvent::Log {
                level: "info".into(),
                message: "hi".into(),
                target: None,
            },
        };
        let row = record_to_row(&rec);
        assert_eq!(row.ts, "03:07:09");
        assert_eq!(row.kind, "ark.core.log");
    }

    #[test]
    fn handle_key_j_k_scroll_and_follow_pause() {
        let mut s = LogState::new("id", None);
        assert!(s.follow);
        handle_key(&mut s, KeyCode::Char('j'));
        assert_eq!(s.scroll_offset, 1);
        assert!(s.follow);
        handle_key(&mut s, KeyCode::Char('k'));
        assert_eq!(s.scroll_offset, 0);
        assert!(!s.follow);
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
    fn handle_key_capital_g_resumes_follow() {
        let mut s = LogState::new("id", None);
        s.follow = false;
        s.scroll_offset = 5;
        handle_key(&mut s, KeyCode::Char('G'));
        assert!(s.follow);
        assert_eq!(s.scroll_offset, usize::MAX);
    }

    #[test]
    fn color_for_kind_error_is_bold_red_when_color_allowed() {
        if no_color() {
            return;
        }
        let style = color_for_kind("ark.core.error", "");
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn color_for_kind_session_ended_is_yellow() {
        if no_color() {
            return;
        }
        assert_eq!(
            color_for_kind("ark.core.session_ended", "").fg,
            Some(Color::Yellow)
        );
    }

    #[test]
    fn color_for_kind_ext_tool_use_is_cyan() {
        if no_color() {
            return;
        }
        assert_eq!(
            color_for_kind("claude-code.tool.use", "").fg,
            Some(Color::Cyan)
        );
    }
}
