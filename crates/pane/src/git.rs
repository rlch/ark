//! `ark pane git` — live git status summary.
//!
//! See `context/kits/cavekit-pane-commands.md` R2: branch header with
//! ahead/behind counters, sections for staged / unstaged / untracked, 2-second
//! refresh poll via pane `Tick`, non-repo placeholder, up/down (or j/k)
//! scroll, `NO_COLOR`-aware styling.
//!
//! The porcelain v2 parser is a small hand-rolled parser — the format is
//! stable and documented at <https://git-scm.com/docs/git-status>.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{PaneEvent, PaneFlow, no_color, run_pane};

/// One changed file parsed from porcelain v2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// Primary status code (M, A, D, R, C, U, ?). For ordinary changes this is
    /// the X (index) for staged and Y (worktree) for unstaged entries; see
    /// [`parse_porcelain_v2`] for the split logic.
    pub status: char,
    pub path: String,
}

/// State backing the `ark pane git` widget.
#[derive(Clone, Debug, Default)]
pub struct GitState {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<String>,
    /// `false` when `git rev-parse --git-dir` failed for the cwd. Widget
    /// still runs but shows the non-repo placeholder.
    pub is_repo: bool,
    pub scroll_offset: usize,
    /// Human-readable error from the last git invocation (if any). Shown in
    /// the footer; does not block the pane loop.
    pub last_error: Option<String>,
}

/// Parsed output of `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PorcelainStatus {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<String>,
}

/// Hand-rolled porcelain v2 parser.
///
/// Lines of interest:
/// - `# branch.head <name>` — current branch (or `(detached)`)
/// - `# branch.ab +N -M` — ahead / behind counters
/// - `1 XY <sub> <mH> <mI> <mW> <hH> <hI> <path>` — ordinary change
/// - `2 XY <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path><tab><origPath>` — rename/copy
/// - `? <path>` — untracked
/// - `! <path>` — ignored (skipped)
/// - `u XY ...` — unmerged (emitted as staged with status `U`)
///
/// X = index status, Y = worktree status. A file shows up in `staged` when
/// X != `.`, and in `unstaged` when Y != `.`. Lines we don't recognize are
/// silently skipped per R2 ("parse robustly; skip lines you don't recognize").
pub fn parse_porcelain_v2(input: &str) -> PorcelainStatus {
    let mut out = PorcelainStatus::default();

    for raw in input.lines() {
        if raw.is_empty() {
            continue;
        }
        // Header lines.
        if let Some(rest) = raw.strip_prefix("# ") {
            if let Some(name) = rest.strip_prefix("branch.head ") {
                out.branch = Some(name.trim().to_string());
            } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                // ab = "+N -M"
                let mut parts = ab.split_whitespace();
                if let Some(a) = parts.next() {
                    out.ahead = a.trim_start_matches('+').parse().unwrap_or(0);
                }
                if let Some(b) = parts.next() {
                    out.behind = b.trim_start_matches('-').parse().unwrap_or(0);
                }
            }
            continue;
        }

        let mut parts = raw.splitn(2, ' ');
        let tag = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");

        match tag {
            "1" => {
                // "XY <sub> <mH> <mI> <mW> <hH> <hI> <path>"
                if let Some(fc) = parse_ordinary(rest, false) {
                    push_change(&mut out, fc);
                }
            }
            "2" => {
                // Rename/copy: same prefix + <Xscore> then path<TAB>origPath
                if let Some(fc) = parse_ordinary(rest, true) {
                    push_change(&mut out, fc);
                }
            }
            "u" => {
                // Unmerged — treat as staged with status 'U'.
                if let Some(last) = rest.split_whitespace().last() {
                    out.staged.push(FileChange {
                        status: 'U',
                        path: last.to_string(),
                    });
                }
            }
            "?" => {
                out.untracked.push(rest.to_string());
            }
            "!" => { /* ignored */ }
            _ => { /* unrecognized — skip */ }
        }
    }

    out
}

/// Parse the body of a `1 ...` or `2 ...` porcelain v2 line. Returns a pair
/// conceptually (index_change, worktree_change) encoded as a single
/// [`FileChange`] stamped with both X and Y via the `push_change` helper.
///
/// Actually: returns a struct with `.x`, `.y`, `.path` because ordinary lines
/// can contribute to both staged and unstaged sections.
struct OrdinaryEntry {
    x: char,
    y: char,
    path: String,
}

fn parse_ordinary(rest: &str, is_rename: bool) -> Option<OrdinaryEntry> {
    // Expected tokens before path:
    //   "1": XY sub mH mI mW hH hI <path>       (7 tokens then path)
    //   "2": XY sub mH mI mW hH hI Xscore <path><TAB><orig>  (8 tokens then path)
    let need = if is_rename { 8 } else { 7 };
    // Walk word-by-word, consuming `need` whitespace-separated tokens, then
    // the remainder is the path (for renames: path<TAB>origPath — we only
    // want the new path, which is before the TAB).
    let mut cursor = rest;
    for _ in 0..need {
        let (_, rem) = eat_token(cursor)?;
        cursor = rem;
    }
    let path_part = cursor.trim_start();
    if path_part.is_empty() {
        return None;
    }
    // For renames, strip "\t<orig>" suffix.
    let path = match path_part.split_once('\t') {
        Some((new, _orig)) => new.to_string(),
        None => path_part.to_string(),
    };
    // First token back in `rest` is the XY status.
    let xy = rest.split_whitespace().next()?;
    let mut chars = xy.chars();
    let x = chars.next()?;
    let y = chars.next()?;
    Some(OrdinaryEntry { x, y, path })
}

fn eat_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(i) => Some((&s[..i], &s[i..])),
        None => Some((s, "")),
    }
}

fn push_change(out: &mut PorcelainStatus, e: OrdinaryEntry) {
    if e.x != '.' {
        out.staged.push(FileChange {
            status: e.x,
            path: e.path.clone(),
        });
    }
    if e.y != '.' {
        out.unstaged.push(FileChange {
            status: e.y,
            path: e.path,
        });
    }
}

/// Detect whether `cwd` is inside a git repository. Pure over the exit code
/// of `git rev-parse --git-dir` (0 = repo, anything else = not a repo).
pub fn is_repo_from_exit_code(code: Option<i32>) -> bool {
    matches!(code, Some(0))
}

/// Apply `state` from a freshly-parsed status, preserving scroll offset.
pub fn apply_status(state: &mut GitState, status: PorcelainStatus) {
    state.branch = status.branch;
    state.ahead = status.ahead;
    state.behind = status.behind;
    state.staged = status.staged;
    state.unstaged = status.unstaged;
    state.untracked = status.untracked;
    state.last_error = None;
    // Clamp scroll to total rendered row count — simple clamp; render handles
    // exact windowing against viewport height.
    let total = state.staged.len() + state.unstaged.len() + state.untracked.len();
    if state.scroll_offset > total {
        state.scroll_offset = total.saturating_sub(1);
    }
}

/// Run the `ark pane git` widget against `cwd`. Polls git every 2 seconds via
/// `PaneEvent::Tick`; exits on `q`, `Esc`, or `Ctrl+C`.
pub async fn run(cwd: PathBuf) -> anyhow::Result<()> {
    // Initial probe: is this a git repo?
    let is_repo = probe_is_repo(&cwd).await;
    let mut state = GitState {
        is_repo,
        ..Default::default()
    };
    // First refresh (sync-ish in the tokio runtime).
    if is_repo {
        match refresh(&cwd).await {
            Ok(status) => apply_status(&mut state, status),
            Err(e) => state.last_error = Some(e.to_string()),
        }
    }

    let cwd_render = cwd.clone();
    let render = move |f: &mut Frame, s: &GitState| {
        render_frame(f, s, &cwd_render);
    };

    // Tick cadence: app.rs drives ticks at ~100ms; the widget coalesces them
    // and only re-runs git every 2 seconds.
    let mut last_refresh = Instant::now();
    let poll_interval = Duration::from_secs(2);

    let handler = move |s: &mut GitState, ev: PaneEvent| -> PaneFlow {
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
                KeyCode::Home | KeyCode::Char('g') => {
                    s.scroll_offset = 0;
                    PaneFlow::Continue
                }
                _ => PaneFlow::Continue,
            },
            PaneEvent::Resize(_, _) => PaneFlow::Continue,
            PaneEvent::Tick => {
                if !s.is_repo {
                    return PaneFlow::Continue;
                }
                if last_refresh.elapsed() < poll_interval {
                    return PaneFlow::Continue;
                }
                last_refresh = Instant::now();
                // Spawn a blocking refresh; results applied via a small
                // synchronous fallback — we use tokio's current-thread
                // scheduler by running a blocking try_recv pattern. For v1
                // just block the tick briefly (refresh is cheap).
                let cwd = cwd.clone();
                let result = futures::executor::block_on(refresh(&cwd));
                match result {
                    Ok(status) => apply_status(s, status),
                    Err(e) => s.last_error = Some(e.to_string()),
                }
                PaneFlow::Continue
            }
            PaneEvent::Custom(_) => PaneFlow::Continue,
        }
    };

    run_pane(state, render, handler).await
}

async fn probe_is_repo(cwd: &std::path::Path) -> bool {
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .output()
        .await;
    match out {
        Ok(o) => is_repo_from_exit_code(o.status.code()),
        Err(_) => false,
    }
}

async fn refresh(cwd: &std::path::Path) -> anyhow::Result<PorcelainStatus> {
    let out = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(cwd)
        .output()
        .await?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git status failed (exit {:?})",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_porcelain_v2(&stdout))
}

/// Build the header line: `branch <name>  ↑N ↓M`.
pub fn header_line(state: &GitState) -> String {
    let branch = state.branch.as_deref().unwrap_or("(detached)");
    format!("branch {}  ↑{} ↓{}", branch, state.ahead, state.behind)
}

fn style_for(color: Color) -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    }
}

fn render_frame(f: &mut Frame, s: &GitState, cwd: &std::path::Path) {
    let area = f.area();

    if !s.is_repo {
        let p = Paragraph::new(format!("not a git repository\n\ncwd: {}", cwd.display())).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ark pane git "),
        );
        f.render_widget(p, area);
        return;
    }

    // Layout: header (3) + body (flex) + footer (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Header
    let header_text = header_line(s);
    let header = Paragraph::new(header_text)
        .style(style_for(Color::Cyan))
        .block(Block::default().borders(Borders::ALL).title(" git "));
    f.render_widget(header, chunks[0]);

    // Body: build lines for three sections in order.
    let lines = build_body_lines(s);
    let body_area = chunks[1];
    let viewport = body_area.height.saturating_sub(2) as usize; // borders
    let offset = s
        .scroll_offset
        .min(lines.len().saturating_sub(viewport.max(1)));
    let end = (offset + viewport).min(lines.len());
    let visible: Vec<Line> = lines[offset..end].to_vec();

    let mut title = format!(
        " staged {} · unstaged {} · untracked {} ",
        s.staged.len(),
        s.unstaged.len(),
        s.untracked.len()
    );
    let truncated = lines.len().saturating_sub(end);
    if truncated > 0 {
        title.push_str(&format!("({truncated} more) "));
    }
    let body = Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(body, body_area);

    // Footer status line.
    let footer_text = match &s.last_error {
        Some(e) => format!("q quit · j/k scroll · ERR: {e}"),
        None => "q quit · j/k scroll · auto-refresh 2s".to_string(),
    };
    let footer = Paragraph::new(footer_text);
    f.render_widget(footer, render_in(chunks[2], 1));
}

fn render_in(r: Rect, _left_pad: u16) -> Rect {
    r
}

/// Build the list of rendered lines for the body (used by tests + render).
pub fn build_body_lines(s: &GitState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    if !s.staged.is_empty() {
        lines.push(Line::styled(
            format!("staged ({})", s.staged.len()),
            style_for(Color::Green),
        ));
        for fc in &s.staged {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", fc.status), style_for(Color::Green)),
                Span::raw(fc.path.clone()),
            ]));
        }
    }

    if !s.unstaged.is_empty() {
        lines.push(Line::styled(
            format!("unstaged ({})", s.unstaged.len()),
            style_for(Color::Yellow),
        ));
        for fc in &s.unstaged {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", fc.status), style_for(Color::Yellow)),
                Span::raw(fc.path.clone()),
            ]));
        }
    }

    if !s.untracked.is_empty() {
        lines.push(Line::styled(
            format!("untracked ({})", s.untracked.len()),
            style_for(Color::Blue),
        ));
        for path in &s.untracked {
            lines.push(Line::from(vec![
                Span::styled("  ? ", style_for(Color::Blue)),
                Span::raw(path.clone()),
            ]));
        }
    }

    if lines.is_empty() {
        lines.push(Line::raw("working tree clean"));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = "\
# branch.oid 0123456789abcdef0123456789abcdef01234567
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -1
1 .M N... 100644 100644 100644 abc def src/lib.rs
1 M. N... 100644 100644 100644 abc def src/main.rs
1 MM N... 100644 100644 100644 abc def src/both.rs
1 A. N... 100644 100644 100644 abc def src/added.rs
1 .D N... 100644 100644 100644 abc def src/removed.rs
2 R. N... 100644 100644 100644 abc def R100 new_path.rs\told_path.rs
u UU N... 100644 100644 100644 100644 aa bb cc merge_me.rs
? untracked.txt
? another/untracked.rs
! ignored.txt
";

    #[test]
    fn parse_golden_branch_and_ab() {
        let s = parse_porcelain_v2(GOLDEN);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 1);
    }

    #[test]
    fn parse_golden_staged_split() {
        let s = parse_porcelain_v2(GOLDEN);
        // Staged: M (main), M (both), A (added), R (new_path), U (merge_me)
        let paths: Vec<_> = s.staged.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"), "staged: {paths:?}");
        assert!(paths.contains(&"src/both.rs"), "staged: {paths:?}");
        assert!(paths.contains(&"src/added.rs"), "staged: {paths:?}");
        assert!(paths.contains(&"new_path.rs"), "staged: {paths:?}");
        assert!(paths.contains(&"merge_me.rs"), "staged: {paths:?}");
        // Statuses: M, M, A, R, U
        let statuses: Vec<_> = s.staged.iter().map(|c| c.status).collect();
        assert!(statuses.contains(&'M'));
        assert!(statuses.contains(&'A'));
        assert!(statuses.contains(&'R'));
        assert!(statuses.contains(&'U'));
    }

    #[test]
    fn parse_golden_unstaged_split() {
        let s = parse_porcelain_v2(GOLDEN);
        // Unstaged: M (lib), M (both), D (removed)
        let paths: Vec<_> = s.unstaged.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"src/lib.rs"), "unstaged: {paths:?}");
        assert!(paths.contains(&"src/both.rs"), "unstaged: {paths:?}");
        assert!(paths.contains(&"src/removed.rs"), "unstaged: {paths:?}");
        assert_eq!(s.unstaged.len(), 3);
    }

    #[test]
    fn parse_golden_untracked_and_ignored_skipped() {
        let s = parse_porcelain_v2(GOLDEN);
        assert_eq!(s.untracked.len(), 2);
        assert!(s.untracked.iter().any(|p| p == "untracked.txt"));
        assert!(s.untracked.iter().any(|p| p == "another/untracked.rs"));
        // '!' ignored lines should not pollute any bucket.
        assert!(!s.untracked.iter().any(|p| p == "ignored.txt"));
    }

    #[test]
    fn parse_empty_input_is_default() {
        let s = parse_porcelain_v2("");
        assert_eq!(s, PorcelainStatus::default());
    }

    #[test]
    fn parse_unknown_line_is_skipped() {
        let input = "# branch.head x\ngibberish line that is nonsense\n? real.txt\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.branch.as_deref(), Some("x"));
        assert_eq!(s.untracked, vec!["real.txt"]);
    }

    #[test]
    fn parse_detached_head_branch_absent() {
        // `# branch.head (detached)` is literally the branch name git emits.
        let input = "# branch.head (detached)\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.branch.as_deref(), Some("(detached)"));
    }

    #[test]
    fn is_repo_from_exit_code_zero_is_true() {
        assert!(is_repo_from_exit_code(Some(0)));
        assert!(!is_repo_from_exit_code(Some(128)));
        assert!(!is_repo_from_exit_code(None));
    }

    #[test]
    fn apply_status_resets_scroll_when_list_shrinks() {
        let mut state = GitState {
            is_repo: true,
            scroll_offset: 99,
            ..Default::default()
        };
        apply_status(&mut state, PorcelainStatus::default());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn header_line_includes_branch_and_counts() {
        let s = GitState {
            is_repo: true,
            branch: Some("main".into()),
            ahead: 5,
            behind: 3,
            ..Default::default()
        };
        let h = header_line(&s);
        assert!(h.contains("main"));
        assert!(h.contains("5"));
        assert!(h.contains("3"));
    }

    #[test]
    fn build_body_lines_clean_tree_has_placeholder() {
        let s = GitState {
            is_repo: true,
            ..Default::default()
        };
        let lines = build_body_lines(&s);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn build_body_lines_sections_in_order() {
        let mut s = GitState {
            is_repo: true,
            ..Default::default()
        };
        s.staged.push(FileChange {
            status: 'M',
            path: "a".into(),
        });
        s.unstaged.push(FileChange {
            status: 'M',
            path: "b".into(),
        });
        s.untracked.push("c".into());
        let lines = build_body_lines(&s);
        // staged header + 1 + unstaged header + 1 + untracked header + 1 = 6
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn header_line_detached_head_without_branch_shows_placeholder() {
        // State with no branch set (e.g. detached HEAD that the parser
        // couldn't resolve) must still produce a valid one-line header.
        let s = GitState {
            is_repo: true,
            branch: None,
            ahead: 0,
            behind: 0,
            ..Default::default()
        };
        let h = header_line(&s);
        assert!(h.contains("(detached)"), "header: {h:?}");
        assert!(h.contains("↑0"));
        assert!(h.contains("↓0"));
    }

    #[test]
    fn git_render_frame_handles_multiple_sizes() {
        // SIGWINCH simulation: render the same state into TestBackends of
        // different sizes. ratatui lays out against f.area(), so any layout
        // panic here would crash the real pane on terminal resize.
        use ratatui::{Terminal, backend::TestBackend};

        let mut s = GitState {
            is_repo: true,
            branch: Some("main".into()),
            ahead: 1,
            behind: 0,
            ..Default::default()
        };
        // Populate each section so render walks all branches.
        s.staged.push(FileChange {
            status: 'M',
            path: "a.rs".into(),
        });
        s.unstaged.push(FileChange {
            status: 'M',
            path: "b.rs".into(),
        });
        s.untracked.push("c.rs".into());

        for (w, h) in [(40u16, 10u16), (80, 24), (120, 40)] {
            let backend = TestBackend::new(w, h);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| render_frame(f, &s, std::path::Path::new("/tmp")))
                .unwrap();
            let buf = term.backend().buffer().clone();
            assert_eq!(buf.area.width, w);
            assert_eq!(buf.area.height, h);
        }
    }

    #[test]
    fn apply_status_preserves_scroll_when_within_bounds() {
        // Contrasts with the existing "resets_scroll_when_list_shrinks" case.
        // When the new list is long enough to keep the current offset valid,
        // the offset must survive the refresh.
        let mut state = GitState {
            is_repo: true,
            scroll_offset: 3,
            ..Default::default()
        };
        let mut status = PorcelainStatus::default();
        for i in 0..10 {
            status.untracked.push(format!("f{i}"));
        }
        apply_status(&mut state, status);
        assert_eq!(state.scroll_offset, 3);
        assert_eq!(state.untracked.len(), 10);
    }
}
