//! `ark pane diff` — live-refreshing git diff display via `delta`.
//!
//! See `context/kits/cavekit-pane-commands.md` R1:
//! - Watches `<cwd>/.git/index` and the working tree (recursive notify on cwd).
//! - On change, debounces per `config.diff.debounce_ms` (default 100ms in the
//!   plan; R1 says default 300ms — caller passes the resolved value).
//! - Pipes `git diff --no-color HEAD` through
//!   `delta --paging=never --side-by-side --line-numbers`; when `delta` isn't
//!   on PATH, falls back to raw git output and warns once.
//! - Renders ANSI-escaped output via `ansi-to-tui` into a ratatui `Paragraph`.
//! - Scroll: j/k/arrows = ±1, PgUp/PgDn = ±10, g = top, G = bottom.
//! - Non-repo cwd: placeholder text + event loop still runs (Ctrl+C/q to quit).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use crossterm::event::KeyCode;
use notify::{RecursiveMode, Watcher};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Text,
    widgets::{Block, Borders, Paragraph},
};
use tokio::process::Command;

use crate::app::{PaneEvent, PaneFlow, no_color, run_pane};

/// Mutable widget state for `ark pane diff`.
pub struct DiffState {
    /// `false` when `git rev-parse --git-dir` failed at startup. Pane still
    /// runs (Ctrl+C / q exits), but only the placeholder is shown.
    pub is_repo: bool,
    /// Raw ANSI-escaped diff text — captured stdout from `delta` (or from
    /// `git diff` when `delta` is unavailable).
    pub last_diff: String,
    /// Vertical scroll offset (lines).
    pub scroll_offset: u16,
    /// Set by the notify callback; cleared by the debounced tick handler when
    /// it kicks off a refresh.
    pub dirty: bool,
    /// Last time the diff pipeline ran (used for debounce gating).
    pub last_refresh: Instant,
    /// Set once at startup. When `false`, the pipeline skips delta and uses
    /// raw `git diff` output. A `tracing::warn!` fires once at startup.
    pub delta_available: bool,
    /// Last error from the diff pipeline (shown in footer; non-fatal).
    pub last_error: Option<String>,
    /// Placeholder text when `is_repo == false`.
    pub placeholder: String,
}

impl DiffState {
    /// Construct an in-repo state with the given delta availability.
    pub fn new_repo(delta_available: bool) -> Self {
        Self {
            is_repo: true,
            last_diff: String::new(),
            scroll_offset: 0,
            // Initial state is "dirty" so the first tick triggers a refresh
            // (the run() function performs an initial refresh too, this is
            // belt-and-suspenders for tests / callers that build state by hand).
            dirty: true,
            last_refresh: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            delta_available,
            last_error: None,
            placeholder: String::new(),
        }
    }

    /// Construct a non-repo state with a placeholder message.
    pub fn new_non_repo() -> Self {
        Self {
            is_repo: false,
            last_diff: String::new(),
            scroll_offset: 0,
            dirty: false,
            last_refresh: Instant::now(),
            delta_available: false,
            last_error: None,
            placeholder: "not a git repository — ark pane diff needs a git repo".to_string(),
        }
    }
}

/// Detect whether `cwd` is inside a git repository. Pure over an exit code:
/// `Some(0)` → in a repo, `Some(non-zero)` → not a repo, `None` → spawn failed
/// (treat as non-repo so the pane still launches with a placeholder).
pub fn is_git_repo(code: Option<i32>) -> bool {
    matches!(code, Some(0))
}

/// Pure debounce gate. Given the dirty flag, the time of the last refresh,
/// the configured debounce window, and "now", decides whether the tick
/// handler should kick off another diff refresh.
pub fn should_refresh(dirty: bool, last_refresh: Instant, debounce_ms: u64, now: Instant) -> bool {
    dirty && now.duration_since(last_refresh) >= Duration::from_millis(debounce_ms)
}

/// Pure scroll-offset clamp. Given the requested offset, the total number of
/// rendered lines, and the viewport height, returns an offset that keeps the
/// last viewport-height lines on screen.
pub fn clamp_scroll(offset: u16, line_count: u16, viewport: u16) -> u16 {
    let max = line_count.saturating_sub(viewport.max(1));
    offset.min(max)
}

/// Run `git rev-parse --git-dir` in `cwd`, returning whether we're in a repo.
async fn probe_is_repo(cwd: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .output()
        .await;
    match out {
        Ok(o) => is_git_repo(o.status.code()),
        Err(_) => false,
    }
}

/// Probe for a `delta` binary on PATH. Returns `true` when `which delta`
/// succeeds with exit code 0.
async fn probe_delta_available() -> bool {
    let out = Command::new("which").arg("delta").output().await;
    match out {
        Ok(o) => o.status.success() && !o.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Run the diff pipeline once. With `delta`: pipes `git diff --color=always
/// HEAD` through `delta --paging=never --side-by-side --line-numbers`. Without
/// `delta`: returns the raw `git diff --no-color HEAD` output.
async fn run_pipeline(cwd: &Path, delta_available: bool) -> anyhow::Result<String> {
    if delta_available {
        // Pull colored diff so delta has something to recolor (delta passes
        // through ANSI it doesn't touch and re-styles the rest).
        let git_out = Command::new("git")
            .args(["diff", "--color=always", "HEAD"])
            .current_dir(cwd)
            .output()
            .await?;
        if !git_out.status.success() && git_out.stdout.is_empty() {
            return Err(anyhow::anyhow!(
                "git diff failed (exit {:?}): {}",
                git_out.status.code(),
                String::from_utf8_lossy(&git_out.stderr)
            ));
        }

        // Spawn delta with stdin piped, write git's stdout, capture delta's
        // stdout.
        use std::process::Stdio;
        let mut child = Command::new("delta")
            .args(["--paging=never", "--side-by-side", "--line-numbers"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(cwd)
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            // Best-effort write; if delta closes early we still want stdout.
            let _ = stdin.write_all(&git_out.stdout).await;
            let _ = stdin.shutdown().await;
        }

        let out = child.wait_with_output().await?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        // Fallback: raw uncolored git diff.
        let out = Command::new("git")
            .args(["diff", "--no-color", "HEAD"])
            .current_dir(cwd)
            .output()
            .await?;
        if !out.status.success() && out.stdout.is_empty() {
            return Err(anyhow::anyhow!(
                "git diff failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

/// Spawn the notify watcher: watches `<cwd>` recursively + `<cwd>/.git/index`
/// (covered by the recursive watch already, but the separate-path fallback
/// kept here as documentation of intent). Returned guard must be held alive.
fn spawn_watcher(cwd: PathBuf, dirty: Arc<Mutex<bool>>) -> Option<notify::RecommendedWatcher> {
    let dirty_cb = dirty.clone();
    let res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(_ev) = res else { return };
        if let Ok(mut f) = dirty_cb.lock() {
            *f = true;
        }
    });
    let Ok(mut watcher) = res else {
        tracing::warn!("failed to create notify watcher — live diff disabled");
        return None;
    };
    if let Err(e) = watcher.watch(&cwd, RecursiveMode::Recursive) {
        tracing::warn!("failed to watch {}: {e}", cwd.display());
        // Still return the (unwatched) watcher so the type lives long enough;
        // tick will just never fire from notify, but Ctrl+C still works.
    }
    Some(watcher)
}

/// Run the `ark pane diff` widget against `cwd`.
///
/// Wiring:
/// 1. Probe is-repo + delta-available at startup.
/// 2. Seed `last_diff` with one synchronous pipeline run.
/// 3. Spawn notify watcher → flips `dirty`.
/// 4. Tick handler debounces per `debounce_ms`; when dirty + window elapsed,
///    re-runs the pipeline (synchronous via futures::executor::block_on, the
///    same pattern used in `git.rs`).
pub async fn run(cwd: PathBuf, debounce_ms: u64) -> anyhow::Result<()> {
    let is_repo = probe_is_repo(&cwd).await;

    if !is_repo {
        let state = DiffState::new_non_repo();
        let cwd_render = cwd.clone();
        let render = move |f: &mut Frame, s: &DiffState| render_frame(f, s, &cwd_render);
        let handler = move |s: &mut DiffState, ev: PaneEvent| -> PaneFlow {
            handle_event_pure(s, ev, debounce_ms, &PathBuf::new())
        };
        return run_pane(state, render, handler).await;
    }

    let delta_available = probe_delta_available().await;
    if !delta_available {
        tracing::warn!(
            "delta binary not found on PATH — falling back to raw git diff output (install dandavison/delta for syntax-highlighted diffs)"
        );
    }

    let mut state = DiffState::new_repo(delta_available);

    // Initial refresh.
    match run_pipeline(&cwd, delta_available).await {
        Ok(out) => {
            state.last_diff = out;
            state.dirty = false;
            state.last_refresh = Instant::now();
        }
        Err(e) => {
            state.last_error = Some(e.to_string());
        }
    }

    // Watcher → dirty flag. Drained by tick handler.
    let dirty = Arc::new(Mutex::new(false));
    let _watcher = spawn_watcher(cwd.clone(), dirty.clone());

    let cwd_render = cwd.clone();
    let render = move |f: &mut Frame, s: &DiffState| render_frame(f, s, &cwd_render);

    let cwd_handler = cwd.clone();
    let handler = move |s: &mut DiffState, ev: PaneEvent| -> PaneFlow {
        // Drain notify dirty flag into state on every tick; pure handler does
        // the rest (debounce + scroll + quit).
        if let PaneEvent::Tick = ev {
            if let Ok(mut f) = dirty.lock() {
                if *f {
                    s.dirty = true;
                    *f = false;
                }
            }
        }
        handle_event_pure(s, ev, debounce_ms, &cwd_handler)
    };

    run_pane(state, render, handler).await
}

/// Pure-ish event handler: scroll + quit + (when running in-repo) debounce →
/// pipeline kick. Extracted so tests can drive it without crossterm/tokio
/// process spawning. `cwd` is empty in non-repo mode (Tick is a no-op then).
pub fn handle_event_pure(
    s: &mut DiffState,
    ev: PaneEvent,
    debounce_ms: u64,
    cwd: &Path,
) -> PaneFlow {
    match ev {
        PaneEvent::Key(k) => match k.code {
            KeyCode::Char('q') | KeyCode::Esc => PaneFlow::Quit,
            KeyCode::Char('j') | KeyCode::Down => {
                s.scroll_offset = s.scroll_offset.saturating_add(1);
                PaneFlow::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                s.scroll_offset = s.scroll_offset.saturating_sub(1);
                PaneFlow::Continue
            }
            KeyCode::PageDown => {
                s.scroll_offset = s.scroll_offset.saturating_add(10);
                PaneFlow::Continue
            }
            KeyCode::PageUp => {
                s.scroll_offset = s.scroll_offset.saturating_sub(10);
                PaneFlow::Continue
            }
            KeyCode::Char('g') | KeyCode::Home => {
                s.scroll_offset = 0;
                PaneFlow::Continue
            }
            KeyCode::Char('G') | KeyCode::End => {
                // Sentinel: render clamps to last full screen.
                s.scroll_offset = u16::MAX;
                PaneFlow::Continue
            }
            _ => PaneFlow::Continue,
        },
        PaneEvent::Resize(_, _) => PaneFlow::Continue,
        PaneEvent::Tick => {
            if !s.is_repo {
                return PaneFlow::Continue;
            }
            if !should_refresh(s.dirty, s.last_refresh, debounce_ms, Instant::now()) {
                return PaneFlow::Continue;
            }
            // Run pipeline synchronously inside the tick (it's fast for normal
            // diffs; matches the `git.rs` pattern).
            let result = futures::executor::block_on(run_pipeline(cwd, s.delta_available));
            s.last_refresh = Instant::now();
            s.dirty = false;
            match result {
                Ok(out) => {
                    s.last_diff = out;
                    s.last_error = None;
                }
                Err(e) => {
                    s.last_error = Some(e.to_string());
                }
            }
            PaneFlow::Continue
        }
        PaneEvent::Custom(_) => PaneFlow::Continue,
    }
}

fn header_style() -> Style {
    if no_color() {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    }
}

fn render_frame(f: &mut Frame, s: &DiffState, cwd: &Path) {
    let area = f.area();

    if !s.is_repo {
        let p = Paragraph::new(format!("{}\n\ncwd: {}", s.placeholder, cwd.display())).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ark pane diff "),
        );
        f.render_widget(p, area);
        return;
    }

    // Header (3) + body (flex) + footer (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Paragraph::new(format!("diff  {}", cwd.display()))
        .style(header_style())
        .block(Block::default().borders(Borders::ALL).title(" git diff "));
    f.render_widget(header, chunks[0]);

    // Body — convert ANSI to ratatui Text.
    let body_area = chunks[1];
    let viewport = body_area.height.saturating_sub(2); // borders
    let text: Text = if s.last_diff.is_empty() {
        Text::raw("Waiting for first edit…")
    } else {
        // ansi-to-tui returns a Result; on parse failure fall back to raw.
        s.last_diff
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(s.last_diff.clone()))
    };
    let line_count = u16::try_from(text.lines.len()).unwrap_or(u16::MAX);
    let offset = clamp_scroll(s.scroll_offset, line_count, viewport);

    let body = Paragraph::new(text).scroll((offset, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} lines ", line_count)),
    );
    f.render_widget(body, body_area);

    // Footer
    let delta_tag = if s.delta_available { "delta" } else { "raw" };
    let footer_text = match &s.last_error {
        Some(e) => format!("q quit · j/k scroll · PgUp/PgDn ±10 · g/G · ERR: {e}"),
        None => format!("q quit · j/k scroll · PgUp/PgDn ±10 · g/G top/bot · {delta_tag}"),
    };
    let footer = Paragraph::new(footer_text);
    f.render_widget(footer, chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn is_git_repo_zero_is_true_others_false() {
        assert!(is_git_repo(Some(0)));
        assert!(!is_git_repo(Some(1)));
        assert!(!is_git_repo(Some(128)));
        assert!(!is_git_repo(None));
    }

    #[test]
    fn diff_state_new_repo_initial_state() {
        let s = DiffState::new_repo(true);
        assert!(s.is_repo);
        assert!(s.delta_available);
        assert!(s.last_diff.is_empty());
        assert_eq!(s.scroll_offset, 0);
        // Initial state is dirty so first tick triggers refresh.
        assert!(s.dirty);
        assert!(s.last_error.is_none());
        assert!(s.placeholder.is_empty());
    }

    #[test]
    fn diff_state_new_non_repo_populates_placeholder() {
        let s = DiffState::new_non_repo();
        assert!(!s.is_repo);
        assert!(!s.delta_available);
        assert!(!s.dirty);
        assert!(!s.placeholder.is_empty());
        assert!(s.placeholder.contains("not a git repository"));
    }

    #[test]
    fn should_refresh_requires_dirty() {
        let now = Instant::now();
        let past = now.checked_sub(Duration::from_secs(1)).unwrap();
        // Window elapsed but not dirty → no refresh.
        assert!(!should_refresh(false, past, 100, now));
        // Dirty + window elapsed → refresh.
        assert!(should_refresh(true, past, 100, now));
    }

    #[test]
    fn should_refresh_requires_debounce_window() {
        let now = Instant::now();
        let just_now = now.checked_sub(Duration::from_millis(10)).unwrap();
        // Dirty but window not elapsed (10ms < 100ms) → no refresh.
        assert!(!should_refresh(true, just_now, 100, now));
        // Dirty + exactly at window boundary → refresh.
        let at_boundary = now.checked_sub(Duration::from_millis(100)).unwrap();
        assert!(should_refresh(true, at_boundary, 100, now));
    }

    #[test]
    fn clamp_scroll_keeps_last_viewport_visible() {
        // 100 lines, viewport 20 → max offset 80.
        assert_eq!(clamp_scroll(0, 100, 20), 0);
        assert_eq!(clamp_scroll(50, 100, 20), 50);
        assert_eq!(clamp_scroll(80, 100, 20), 80);
        assert_eq!(clamp_scroll(81, 100, 20), 80);
        assert_eq!(clamp_scroll(u16::MAX, 100, 20), 80);
        // Fewer lines than viewport → clamp to 0.
        assert_eq!(clamp_scroll(5, 10, 20), 0);
        // Zero viewport guards against div-by-zero panics; treats as 1.
        // 100 lines, viewport-1 → max offset 99, request 50 → 50.
        assert_eq!(clamp_scroll(50, 100, 0), 50);
        assert_eq!(clamp_scroll(u16::MAX, 100, 0), 99);
    }

    #[test]
    fn handle_event_pure_quit_keys() {
        let mut s = DiffState::new_repo(false);
        let cwd = PathBuf::from(".");
        assert_eq!(
            handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Char('q'))), 100, &cwd),
            PaneFlow::Quit
        );
        assert_eq!(
            handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Esc)), 100, &cwd),
            PaneFlow::Quit
        );
    }

    #[test]
    fn handle_event_pure_scroll_keys() {
        let mut s = DiffState::new_repo(false);
        let cwd = PathBuf::from(".");
        // Down +1
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Down)), 100, &cwd);
        assert_eq!(s.scroll_offset, 1);
        // j +1
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Char('j'))), 100, &cwd);
        assert_eq!(s.scroll_offset, 2);
        // k -1
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Char('k'))), 100, &cwd);
        assert_eq!(s.scroll_offset, 1);
        // Up -1
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Up)), 100, &cwd);
        assert_eq!(s.scroll_offset, 0);
        // PgDn +10
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::PageDown)), 100, &cwd);
        assert_eq!(s.scroll_offset, 10);
        // PgUp -10
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::PageUp)), 100, &cwd);
        assert_eq!(s.scroll_offset, 0);
        // G → MAX (render clamps)
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Char('G'))), 100, &cwd);
        assert_eq!(s.scroll_offset, u16::MAX);
        // g → 0
        handle_event_pure(&mut s, PaneEvent::Key(key(KeyCode::Char('g'))), 100, &cwd);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn handle_event_pure_tick_skips_when_non_repo() {
        let mut s = DiffState::new_non_repo();
        // Even with debounce=0, non-repo state must not block-on git.
        let flow = handle_event_pure(&mut s, PaneEvent::Tick, 0, &PathBuf::new());
        assert_eq!(flow, PaneFlow::Continue);
        // No state mutation expected.
        assert!(s.last_diff.is_empty());
        assert!(s.last_error.is_none());
    }

    #[test]
    fn handle_event_pure_resize_is_noop() {
        let mut s = DiffState::new_repo(false);
        let cwd = PathBuf::from(".");
        let before_offset = s.scroll_offset;
        let flow = handle_event_pure(&mut s, PaneEvent::Resize(80, 24), 100, &cwd);
        assert_eq!(flow, PaneFlow::Continue);
        assert_eq!(s.scroll_offset, before_offset);
    }

    #[test]
    fn handle_event_pure_custom_event_is_noop() {
        // Custom events are reserved for future file-watch dispatch (T-040).
        // Until wired, they must not change state or quit the loop.
        let mut s = DiffState::new_repo(false);
        let before_offset = s.scroll_offset;
        let before_dirty = s.dirty;
        let payload: Box<dyn std::any::Any + Send> = Box::new(());
        let flow = handle_event_pure(&mut s, PaneEvent::Custom(payload), 100, &PathBuf::from("."));
        assert_eq!(flow, PaneFlow::Continue);
        assert_eq!(s.scroll_offset, before_offset);
        assert_eq!(s.dirty, before_dirty);
    }

    #[test]
    fn clamp_scroll_when_lines_exactly_fit_viewport_is_zero() {
        // When rendered line count equals viewport height there is nothing
        // off-screen, so the max offset collapses to zero.
        assert_eq!(clamp_scroll(0, 20, 20), 0);
        assert_eq!(clamp_scroll(5, 20, 20), 0);
        assert_eq!(clamp_scroll(u16::MAX, 20, 20), 0);
    }

    #[test]
    fn diff_render_resize_produces_output_at_multiple_sizes() {
        // SIGWINCH simulation: the real pane loop re-draws via `terminal.draw`
        // on any Resize event. Exercising render_frame against different-sized
        // TestBackends ensures layout code handles small and large viewport
        // dimensions without panicking — any panic here would also crash the
        // real pane on SIGWINCH.
        use ratatui::{Terminal, backend::TestBackend};

        let s = DiffState::new_repo(false);
        let cwd = PathBuf::from("/tmp/fake");

        // Tiny size — header (3) + body min 1 + footer (1) barely fits.
        let backend_small = TestBackend::new(20, 8);
        let mut term = Terminal::new(backend_small).unwrap();
        term.draw(|f| render_frame(f, &s, &cwd)).unwrap();
        let buf_small = term.backend().buffer().clone();
        assert_eq!(buf_small.area.width, 20);
        assert_eq!(buf_small.area.height, 8);

        // "Resize" up to 80x24 — same state, different buffer dimensions.
        let backend_large = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend_large).unwrap();
        term.draw(|f| render_frame(f, &s, &cwd)).unwrap();
        let buf_large = term.backend().buffer().clone();
        assert_eq!(buf_large.area.width, 80);
        assert_eq!(buf_large.area.height, 24);

        // Content should include the title marker in both renders.
        let text_large: String = buf_large.content().iter().map(|c| c.symbol()).collect();
        assert!(text_large.contains("git diff"));
    }
}
