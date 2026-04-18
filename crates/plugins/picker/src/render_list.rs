//! Picker list-screen rendering + key handling (cavekit-plugin-picker R4 / W1).
//!
//! All helpers in this module are pure Rust — no wasm or zellij-tile imports —
//! so they are exhaustively host-testable. The wasm plugin `render` method in
//! [`crate`] calls these helpers and hands the resulting strings to
//! `Text::new(...).color_range(...)` / `print_text_with_coordinates`. The list
//! screen is driven entirely by [`handle_list_key`] which maps a minimal
//! [`KeyInput`] enum to a [`PickerAction`]; the wasm side translates zellij
//! `Key` variants into [`KeyInput`] and then translates actions back into
//! zellij-tile side effects. Keeping that boundary narrow is what makes the
//! screen testable on the host.
//!
//! # Acceptance criteria mapping (`cavekit-plugin-picker.md` R4)
//!
//! - Header line: [`build_header`].
//! - Filter input row: rendered inline by the plugin using
//!   [`ListState::filter`].
//! - Row format `{sel}{icon}{orch}:{name} {progress} {extra} {age}`:
//!   [`format_row`] + [`format_progress`] + [`format_age`] +
//!   [`phase_icon`] / [`phase_extra`].
//! - `[R]` tag for resurrectable: driven by the `is_resurrectable` argument
//!   to [`format_row`].
//! - Selected highlight via `Text::color_range`: the wasm `render` method
//!   applies the range; [`format_row`] returns the raw text (with a `> `
//!   selection marker) so the range is always `0..cols`.
//! - Width-aware truncation + right-aligned progress: [`format_row`] truncates
//!   the middle `orch:name` column with an ellipsis and places progress in
//!   a fixed right-aligned slot before the extra/age trailing columns.
//! - Filter input via nucleo-matcher on `{orch}:{name} {id}`:
//!   [`fuzzy_filter_and_sort`].
//! - Footer hints: [`build_footer`].
//!
//! # Banned crate reminder
//!
//! R1 bans `humantime`, `chrono`, and `serde_json`. [`format_age`] is
//! hand-rolled accordingly. Do not pull those crates in to "clean up" the
//! implementation — the wasm size budget depends on it.

use crate::state::{AgentSummary, ListState, PickerCache};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Actions the list screen can emit in response to a key press.
///
/// Pure data — the wasm side translates these into zellij-tile calls
/// (`switch_session`, `run_command`, `hide_self`, etc.) in T-103+.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Dismiss the picker (`hide_self`).
    Close,
    /// User picked an active agent; switch to its zellij session.
    OpenSession(String),
    /// User pressed `Del` on an active agent; open the kill modal.
    ConfirmKill(String),
    /// `?`: open the help overlay.
    OpenHelp,
    /// `?` / `Esc` on the help overlay: return to the list screen.
    CloseHelp,
    /// `Shift+Del`: bulk-terminate every agent in the cache whose phase is
    /// Done / Failed / Killed / Timeout. The wasm dispatcher iterates the
    /// cache and fires `Kill` over each socket, absorbing per-agent
    /// failures. R9.
    KillAllDoneFailed,
    /// Filter text changed (append/backspace). UI redraws; no side effect.
    FilterChanged,
    /// Selection moved up.
    MoveUp,
    /// Selection moved down.
    MoveDown,
    /// Enter on the list screen when the detail expand-tree should open
    /// instead of switching sessions (T-103). Carries the agent id so the
    /// wasm side can queue the on-demand socket fetch.
    ExpandDetail(String),
    /// Left / Tab / Esc on the detail screen: collapse back to the list.
    CollapseDetail,
    /// User confirmed kill in the W4 modal. `keep_worktree` distinguishes
    /// the lowercase-`y` (keep) vs uppercase-`Y` (wipe) variants the R7
    /// wireframe calls out.
    ///
    /// F-607: both variants dispatch `Kill` (the graceful supervisor
    /// command) — only `keep_worktree` differs between them. The earlier
    /// implementation silently escalated the `Y` variant to `ForceKill`,
    /// which diverged from the R7 legend ("Kill + worktree", not "force
    /// kill"). Force-kill is a separate UX not reachable through this
    /// modal.
    ExecKill {
        /// Agent id to terminate.
        agent_id: String,
        /// `true` = keep worktree on disk after the kill ack;
        /// `false` = also remove the worktree (`remove_worktree=true`
        /// in the socket payload). The kill command itself is always
        /// `Kill` (never `ForceKill`) regardless of this flag.
        keep_worktree: bool,
    },
    /// User cancelled the kill modal (lowercase `n` / Esc).
    CancelKill,
    /// User submitted a new name from the rename prompt. `(agent_id,
    /// new_name)`.
    ExecRename(String, String),
    /// User cancelled the rename prompt (Esc).
    CancelRename,
    /// User pressed `Ctrl+D` on a live agent; the supervisor should forget
    /// the agent (spec.json stays but `Forget` tells the supervisor to
    /// detach from tracking). Fires immediately — no confirm, per T-105.
    ExecForget(String),
    /// User pressed `Ctrl+R` on a live agent; open the rename prompt
    /// focused on that id.
    OpenRenamePrompt(String),
    /// Key ignored (no matching action).
    None,
}

/// Minimal key-input enum — mirrors only the zellij `Key` variants the
/// list screen actually cares about.
///
/// Keeping this isolated from `zellij_tile::prelude::Key` is what lets
/// [`handle_list_key`] live on the host side. The wasm `update` branch
/// maps from `Key` to `KeyInput` with a handful of match arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    /// Arrow up / `k`.
    Up,
    /// Arrow down / `j`.
    Down,
    /// `Enter`: switch to selected agent's session.
    Enter,
    /// `Esc`: close the picker.
    Esc,
    /// `Delete`: open the kill modal on the selected active agent.
    Delete,
    /// `Backspace`: pop the last char from the filter.
    Backspace,
    /// Printable character (letters / digits / space / punctuation that
    /// isn't bound elsewhere). Appended to the filter verbatim unless
    /// matched by a dedicated action below.
    Char(char),
    /// `Ctrl+R` — on the list screen, opens the rename prompt for the
    /// currently selected live agent (T-105 R7).
    CtrlR,
    /// `Ctrl+D` — on the list screen, tells the supervisor to `Forget` the
    /// currently selected live agent (detach; no confirm). T-105 R7.
    CtrlD,
    /// `Tab` — on the detail screen this collapses back to the list; on
    /// the new-agent form it cycles focus forward.
    Tab,
    /// `Shift+Tab` — on the new-agent form it cycles focus backward.
    ShiftTab,
    /// `Left` arrow — on the detail screen this collapses back to the
    /// list; on the new-agent form it cycles the focused radio / dropdown
    /// backward.
    Left,
    /// `Right` arrow — on the new-agent form it cycles the focused radio
    /// / dropdown forward.
    Right,
    /// Vim `j` — move selection down. Distinct from [`Self::Down`] so
    /// tests can assert the two paths independently even though the
    /// action they emit is the same.
    J,
    /// Vim `k` — move selection up.
    K,
    /// Vim `l` — expand the selected row / enter Detail (R9).
    L,
    /// Vim `h` — collapse Detail back to List (R9).
    H,
    /// `Shift+Delete` — bulk-kill every Done/Failed/Killed/Timeout agent
    /// in view (R9).
    ShiftDel,
    /// `/` — enter filter-capture mode (printable chars append to filter
    /// verbatim; `Esc` exits filter mode).
    Slash,
    /// `?` — open/close the help overlay (R9).
    Question,
    /// `Ctrl+C` — close the picker. Mirrors `Esc` when not in filter mode.
    CtrlC,
    /// Any key we don't care about.
    Other,
}

// ---------------------------------------------------------------------------
// Header & footer builders
// ---------------------------------------------------------------------------

/// R4: header line. Counts come from [`PickerCache`].
pub fn build_header(active: usize, resurrectable: usize) -> String {
    format!("ark picker — {active} active, {resurrectable} crashed (press ? for help)")
}

/// R4: footer hints. Kept as one line so width-aware render can drop it
/// gracefully in narrow terminals if needed (T-103 problem, not ours).
pub fn build_footer() -> String {
    "Enter: open │ Del: kill │ ?: help │ Esc: close".to_string()
}

// ---------------------------------------------------------------------------
// Row formatting
// ---------------------------------------------------------------------------

/// Phase → emoji icon (R4 bullet 3).
///
/// Unknown phases fall through to the "running" icon so new orchestrator
/// states don't render as blanks — better to surface something than
/// nothing while the enum catches up.
pub fn phase_icon(phase: &str) -> &'static str {
    match phase {
        "running" => "⟳",
        "stalled" => "⏸",
        "findings" | "review-pending" => "⚠",
        "done" | "Done" => "✓",
        "failed" | "Failed" => "✗",
        "crashed" | "Crashed" => "💀",
        "reviewing" | "Reviewing" => "🔍",
        _ => "⟳",
    }
}

/// Phase-specific "extra" column (R4 wireframe column 4).
///
/// Values mirror the W1 wireframe in the kit. Empty string for phases
/// that don't have an obvious extra string.
pub fn phase_extra(summary: &AgentSummary) -> String {
    match summary.phase.as_str() {
        "running" => match summary.iter {
            Some(i) => format!("iter {i}"),
            None => "running".to_string(),
        },
        "stalled" => "stalled".to_string(),
        "findings" | "review-pending" => "findings".to_string(),
        "done" | "Done" => "done".to_string(),
        "failed" | "Failed" => "failed".to_string(),
        "crashed" | "Crashed" => "crashed".to_string(),
        "reviewing" | "Reviewing" => "reviewing".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => String::new(),
    }
}

/// Format `(done, total)` as `done/total`, or `""` for `None`.
///
/// Separate helper so the width calculation in [`format_row`] can measure
/// the rendered progress string without re-computing it.
pub fn format_progress(progress: Option<(u32, u32)>) -> String {
    match progress {
        Some((done, total)) => format!("{done}/{total}"),
        None => String::new(),
    }
}

/// Hand-rolled humantime for R4 "age" column.
///
/// Units: `s`, `m`, `h`, `d`. We never need higher resolution than whole
/// seconds; we never need lower resolution than days (picker is a
/// live-agent view — if an agent is a week old the operator has bigger
/// problems than the picker's UX).
///
/// `now_ms < then_ms` (clock skew) yields `"0s ago"` — keep it bounded.
/// R1 bans `humantime` and `chrono`; do not swap them in.
pub fn format_age(now_ms: u64, then_ms: u64) -> String {
    let delta_ms = now_ms.saturating_sub(then_ms);
    let secs = delta_ms / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Count chars (not bytes) — field widths care about display length, and
/// all our glyphs are single-width (ASCII plus a handful of BMP emoji
/// that zellij renders as width-1 in practice). A future polish pass
/// could switch this to `unicode-width`, but adding that dep would
/// violate R1's wasm-size budget.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Truncate `s` to at most `max` characters, appending `…` if clipped.
fn truncate_to(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let len = char_len(s);
    if len <= max {
        return s.to_string();
    }
    // Reserve 1 char for the ellipsis.
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

/// Format one row of the list screen.
///
/// Layout (R4 wireframe), from left to right:
///
/// - `sel_marker` — `"> "` if selected, `"  "` otherwise.
/// - `icon` — [`phase_icon`] plus a single space.
/// - `orch:name` — truncated to fit the middle column (ellipsis).
/// - `progress` — right-aligned to 7 chars.
/// - `extra` — phase-specific (`iter 3`, `done`, ...).
/// - `age` — `format_age(now_ms, last_event_at)`; falls back to `started_at`.
/// - `[R]` — trailing tag if `is_resurrectable`.
///
/// `width` is the total terminal column count. The helper right-pads
/// (rather than clipping hard) so `Text::color_range(.., 0..width)`
/// highlights the whole row, which is what R4's "Selected row
/// highlighted via Text.color_range" calls for.
///
/// `is_focused` is the "this is the session I'm currently sitting in"
/// highlight — currently just adds a `•` after the selector marker so
/// visual distinction is present without needing a second color range.
pub fn format_row(
    summary: &AgentSummary,
    is_selected: bool,
    is_focused: bool,
    is_resurrectable: bool,
    now_ms: u64,
    width: usize,
) -> String {
    let sel_marker = if is_selected { "> " } else { "  " };
    let focus_dot = if is_focused { "•" } else { " " };
    let icon = phase_icon(&summary.phase);
    let progress = format_progress(summary.progress);
    let extra = phase_extra(summary);
    // Prefer last_event_at for "age" (matches the wireframe semantics of
    // "how long since we heard from this agent"); fall back to started_at.
    let age = match summary.last_event_at.or(summary.started_at) {
        Some(ts) => format_age(now_ms, ts.saturating_mul(1000)),
        None => String::new(),
    };
    let tag = if is_resurrectable { " [R]" } else { "" };

    let orch_name = format!("{}:{}", summary.orchestrator, summary.name);

    // Budget the middle column. Everything else is essentially fixed-width.
    // prefix = "> • ⟳ " ≈ 6 chars; trailing columns vary with content.
    // Compose trailing first so we know how much room the middle gets.
    let progress_col = format!("{:>7}", progress);
    let trailing = format!("  {progress_col}  {extra}  {age}{tag}");
    let prefix = format!("{sel_marker}{focus_dot}{icon} ");
    let prefix_len = char_len(&prefix);
    let trailing_len = char_len(&trailing);
    // Reserve at least 1 char for the middle column so truncation can
    // always land an ellipsis; if the terminal is absurdly narrow we
    // just let the trailing push out the right edge (better than
    // panicking on underflow).
    let middle_budget = width
        .saturating_sub(prefix_len)
        .saturating_sub(trailing_len)
        .max(1);
    let middle = truncate_to(&orch_name, middle_budget);
    let mut row = format!("{prefix}{middle}{trailing}");

    // Right-pad to `width` so a color_range over 0..width highlights the
    // whole line. If the row is already wider (very narrow terminal,
    // overflow), clip to `width` chars.
    let current = char_len(&row);
    if current < width {
        row.push_str(&" ".repeat(width - current));
    } else if current > width && width > 0 {
        row = row.chars().take(width).collect();
    }
    row
}

// ---------------------------------------------------------------------------
// Fuzzy filter via nucleo-matcher
// ---------------------------------------------------------------------------

/// Build the fuzzy-match haystack for an agent — `{orch}:{name} {id}`.
///
/// Exposed separately so tests can assert the composition without
/// depending on nucleo's scoring internals.
pub fn fuzzy_haystack(summary: &AgentSummary) -> String {
    format!("{}:{} {}", summary.orchestrator, summary.name, summary.id)
}

/// Fuzzy-filter and stably-sort the cache against `query`.
///
/// Returns `(agent_id, score)` pairs in descending score order. Empty
/// query returns all entries (active first, then resurrectable) with a
/// score of `0` in BTreeMap iteration order — that's the "freshly opened
/// picker shows the full list" R4 behaviour.
///
/// Uses [`Matcher::fuzzy_match`] with [`Config::DEFAULT`] — the default
/// is case-insensitive, which matches the case-insensitivity covered by
/// T-100's `filter_matches` tests.
pub fn fuzzy_filter_and_sort(cache: &PickerCache, query: &str) -> Vec<(String, i32)> {
    let mut out: Vec<(String, i32)> = Vec::new();
    if query.is_empty() {
        // Preserve stable BTreeMap iteration order for both halves.
        for id in cache.active.keys() {
            out.push((id.clone(), 0));
        }
        for id in cache.resurrectable.keys() {
            out.push((id.clone(), 0));
        }
        return out;
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    // nucleo-matcher's `ignore_case` config field only lowercases the
    // *haystack* — uppercase letters in the needle still require an
    // exact-case match. R4 wants symmetric case-insensitivity, so we
    // lowercase the needle up-front. This mirrors the T-100
    // `filter_matches` semantics that the "Filter input: fuzzy … case-
    // insensitive" bullet pins down.
    let lowered = query.to_ascii_lowercase();
    let mut needle_buf: Vec<char> = Vec::new();
    let needle = Utf32Str::new(&lowered, &mut needle_buf);

    let mut scored: Vec<(String, i32, usize)> = Vec::new();
    for (idx, summary) in cache
        .active
        .values()
        .chain(cache.resurrectable.values())
        .enumerate()
    {
        let hay_str = fuzzy_haystack(summary);
        let mut hay_buf: Vec<char> = Vec::new();
        let hay = Utf32Str::new(&hay_str, &mut hay_buf);
        if let Some(score) = matcher.fuzzy_match(hay, needle) {
            // score is u16; keep as i32 for the public API so future
            // negative-scoring variants (e.g. demoting dim agents)
            // aren't blocked by the type.
            scored.push((summary.id.clone(), score as i32, idx));
        }
    }
    // Stable sort: primary descending score, secondary ascending original
    // index (so ties keep BTreeMap iteration order).
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(id, s, _)| (id, s)).collect()
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Return the agent id for the currently highlighted row in the
/// post-filter view, and whether that agent is resurrectable.
fn selected_agent<'a>(
    cache: &'a PickerCache,
    state: &ListState,
) -> Option<(&'a str, bool /* is_resurrectable */)> {
    let filtered = fuzzy_filter_and_sort(cache, &state.filter);
    let (id, _score) = filtered.get(state.selected)?;
    let is_res = cache.resurrectable.contains_key(id.as_str());
    // Borrow the id from whichever cache it lives in so the return
    // reference is tied to `cache`.
    if let Some((k, _)) = cache.active.get_key_value(id.as_str()) {
        return Some((k.as_str(), false));
    }
    if let Some((k, _)) = cache.resurrectable.get_key_value(id.as_str()) {
        return Some((k.as_str(), true));
    }
    // Fallback: the id came out of the filter function but the cache
    // maps don't contain it — should be unreachable.
    let _ = is_res;
    None
}

/// Pure key handler for the list screen.
///
/// Mutates `state` as needed (selection / filter) and returns the
/// high-level action to perform. The wasm side maps actions → zellij-tile
/// calls; host tests assert on the returned action without a side effect.
pub fn handle_list_key(state: &mut ListState, cache: &PickerCache, key: KeyInput) -> PickerAction {
    let total = {
        let filtered = fuzzy_filter_and_sort(cache, &state.filter);
        filtered.len()
    };
    // R9: when filter-capture mode is active, Esc only exits filter mode,
    // Backspace pops from the filter, and printable chars append verbatim
    // (no bare-letter hotkey interpretation).
    if state.filter_active {
        match key {
            KeyInput::Esc => {
                state.filter_active = false;
                return PickerAction::FilterChanged;
            }
            KeyInput::Backspace => {
                if state.filter.pop().is_some() {
                    state.selected = 0;
                    return PickerAction::FilterChanged;
                }
                return PickerAction::None;
            }
            KeyInput::Char(c) if !c.is_control() => {
                state.filter.push(c);
                state.selected = 0;
                return PickerAction::FilterChanged;
            }
            KeyInput::Up | KeyInput::K => {
                crate::state::move_selection_up(state, total);
                return PickerAction::MoveUp;
            }
            KeyInput::Down | KeyInput::J => {
                crate::state::move_selection_down(state, total);
                return PickerAction::MoveDown;
            }
            KeyInput::Enter => {
                // Enter in filter mode commits the filter (exit capture)
                // and falls through to the normal Enter handler below.
                state.filter_active = false;
            }
            _ => {
                // All other keys (Ctrl+*, Shift+Del, arrows-with-modifiers,
                // etc.) fall through to the normal handler — operators can
                // still dispatch Ctrl+N / Del etc. while typing a filter.
            }
        }
    }
    match key {
        KeyInput::Esc | KeyInput::CtrlC => PickerAction::Close,
        KeyInput::Up | KeyInput::K => {
            crate::state::move_selection_up(state, total);
            PickerAction::MoveUp
        }
        KeyInput::Down | KeyInput::J => {
            crate::state::move_selection_down(state, total);
            PickerAction::MoveDown
        }
        KeyInput::L | KeyInput::Right => match selected_agent(cache, state) {
            // R9: `l` / → expands into the detail screen for the selected
            // agent. Crashed rows have no supervisor socket to query, so
            // expansion is only meaningful for live entries.
            Some((id, false)) => PickerAction::ExpandDetail(id.to_string()),
            _ => PickerAction::None,
        },
        KeyInput::H | KeyInput::Left => PickerAction::CollapseDetail,
        KeyInput::Slash => {
            state.filter_active = true;
            PickerAction::FilterChanged
        }
        KeyInput::Question => PickerAction::OpenHelp,
        KeyInput::ShiftDel => PickerAction::KillAllDoneFailed,
        KeyInput::Enter => match selected_agent(cache, state) {
            // T-107: Enter on an active (alive) agent switches directly
            // to its zellij session. F-601: the wasm dispatcher passes
            // `summary.session` — the real zellij session id,
            // `ark-{orch}-{name}-{ulid8}` after F-522/F-600. Previously
            // this carried `summary.name` (the bare human label), which
            // named a session that does not exist.
            Some((id, false)) => match cache.active.get(id) {
                Some(summary) => {
                    // F-601: fall back to name ONLY when session is empty,
                    // e.g. an older supervisor that pre-dates F-600 never
                    // stamped the suffixed session onto spec.json.
                    let target = if summary.session.is_empty() {
                        summary.name.clone()
                    } else {
                        summary.session.clone()
                    };
                    PickerAction::OpenSession(target)
                }
                // Cache lookup races are defensive — selected_agent just
                // confirmed the id exists; fall back to OpenSession on the
                // id so we don't silently drop the keystroke.
                None => PickerAction::OpenSession(id.to_string()),
            },
            // Enter on a resurrectable (crashed) agent is a no-op in the
            // list-and-attach-only picker: there is no CLI surface for
            // spawning arbitrary agents anymore, so crashed rows stay as
            // read-only markers until the operator clears them manually.
            Some((_id, true)) => PickerAction::None,
            None => PickerAction::None,
        },
        KeyInput::Delete => match selected_agent(cache, state) {
            Some((id, false)) => PickerAction::ConfirmKill(id.to_string()),
            // Del on a resurrectable is a no-op.
            _ => PickerAction::None,
        },
        KeyInput::Backspace => {
            if state.filter.pop().is_some() {
                // Selection may now point past the new filtered size;
                // clamp via move helpers which saturate correctly.
                state.selected = 0;
                PickerAction::FilterChanged
            } else {
                PickerAction::None
            }
        }
        KeyInput::CtrlR => match selected_agent(cache, state) {
            // R7: Ctrl+R opens the rename prompt on live agents only.
            Some((id, false)) => PickerAction::OpenRenamePrompt(id.to_string()),
            _ => PickerAction::None,
        },
        KeyInput::CtrlD => match selected_agent(cache, state) {
            // R7: Ctrl+D fires Forget immediately (no confirm) on live
            // agents. Crashed agents already ignore the supervisor so a
            // Forget would be a no-op — skip.
            Some((id, false)) => PickerAction::ExecForget(id.to_string()),
            _ => PickerAction::None,
        },
        KeyInput::Char(c) => {
            // R9 bindings (not in filter mode): bare-letter hotkeys take
            // precedence over filter append. Users enter filter mode
            // explicitly with `/`.
            match c {
                '?' => PickerAction::OpenHelp,
                '/' => {
                    state.filter_active = true;
                    PickerAction::FilterChanged
                }
                'j' => {
                    crate::state::move_selection_down(state, total);
                    PickerAction::MoveDown
                }
                'k' => {
                    crate::state::move_selection_up(state, total);
                    PickerAction::MoveUp
                }
                'l' => match selected_agent(cache, state) {
                    Some((id, false)) => PickerAction::ExpandDetail(id.to_string()),
                    _ => PickerAction::None,
                },
                'h' => PickerAction::CollapseDetail,
                // Any other char is ignored in navigation mode. Press
                // `/` to enter filter capture.
                _ => PickerAction::None,
            }
        }
        // Tab/ShiftTab are meaningful on the detail screen but ignored on
        // the list screen itself.
        KeyInput::Tab | KeyInput::ShiftTab | KeyInput::Other => PickerAction::None,
    }
}

/// Return `true` when `phase` is a terminal state (Done / Failed / Killed /
/// Timeout). Exposed for the wasm bulk-kill branch (Shift+Del) which
/// iterates the active cache filtering on terminal phases. Matches the
/// four values the kit calls out plus their lowercased variants.
pub fn is_terminal_phase(phase: &str) -> bool {
    matches!(
        phase,
        "Done" | "done" | "Failed" | "failed" | "Killed" | "killed" | "Timeout" | "timeout"
    )
}

// ---------------------------------------------------------------------------
// Tests — host-side, no wasm.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AgentSummary;

    fn sum(id: &str, name: &str) -> AgentSummary {
        AgentSummary {
            id: id.into(),
            name: name.into(),
            // F-601: default test session mirrors the real-spawn shape
            // `ark-{orch}-{name}-{ulid8}` so Enter-handler assertions
            // exercise the suffixed-session path, not the bare-name
            // fallback.
            session: format!("ark-cavekit-{name}-ABCDEFGH"),
            orchestrator: "cavekit".into(),
            engine: "claude-code".into(),
            phase: "running".into(),
            cwd: "/tmp".into(),
            iter: Some(3),
            started_at: Some(1_000),
            last_event_at: Some(1_000),
            progress: Some((3, 10)),
        }
    }

    // --- format_age ---------------------------------------------------------

    #[test]
    fn format_age_seconds() {
        assert_eq!(format_age(5_000, 0), "5s ago");
    }

    #[test]
    fn format_age_minutes() {
        // 3 minutes = 180s
        assert_eq!(format_age(180_000, 0), "3m ago");
    }

    #[test]
    fn format_age_hours() {
        // 1 hour = 3600s
        assert_eq!(format_age(3_600_000, 0), "1h ago");
    }

    #[test]
    fn format_age_days() {
        // 2 days = 172800s
        assert_eq!(format_age(2 * 86_400_000, 0), "2d ago");
    }

    #[test]
    fn format_age_clock_skew_returns_zero() {
        // then > now → saturate to 0s.
        assert_eq!(format_age(0, 99_999), "0s ago");
    }

    // --- format_progress ----------------------------------------------------

    #[test]
    fn format_progress_none_is_empty() {
        assert_eq!(format_progress(None), "");
    }

    #[test]
    fn format_progress_tuple() {
        assert_eq!(format_progress(Some((3, 10))), "3/10");
    }

    // --- phase_icon / phase_extra ------------------------------------------

    #[test]
    fn phase_icon_covers_known_states() {
        assert_eq!(phase_icon("running"), "⟳");
        assert_eq!(phase_icon("stalled"), "⏸");
        assert_eq!(phase_icon("findings"), "⚠");
        assert_eq!(phase_icon("done"), "✓");
        assert_eq!(phase_icon("failed"), "✗");
        assert_eq!(phase_icon("crashed"), "💀");
        assert_eq!(phase_icon("reviewing"), "🔍");
    }

    #[test]
    fn phase_extra_running_uses_iter() {
        let s = sum("a", "feat");
        assert_eq!(phase_extra(&s), "iter 3");
    }

    // --- format_row ---------------------------------------------------------

    #[test]
    fn format_row_selected_shows_arrow_prefix() {
        let s = sum("abc", "feat");
        let row = format_row(&s, true, false, false, 10_000, 80);
        assert!(row.starts_with("> "), "row = {row:?}");
    }

    #[test]
    fn format_row_unselected_shows_space_prefix() {
        let s = sum("abc", "feat");
        let row = format_row(&s, false, false, false, 10_000, 80);
        assert!(row.starts_with("  "), "row = {row:?}");
    }

    #[test]
    fn format_row_resurrectable_has_r_tag() {
        let s = sum("abc", "old");
        let row = format_row(&s, false, false, true, 10_000, 80);
        assert!(row.contains("[R]"), "row = {row:?}");
    }

    #[test]
    fn format_row_truncates_long_middle() {
        let mut s = sum("abc", "this-is-a-very-long-agent-name-that-cannot-fit");
        s.orchestrator = "cavekit".into();
        let row = format_row(&s, false, false, false, 10_000, 40);
        assert!(row.contains('…'), "expected ellipsis, got {row:?}");
        // Row should be exactly the requested width.
        assert_eq!(row.chars().count(), 40);
    }

    #[test]
    fn format_row_pads_to_width() {
        let s = sum("abc", "feat");
        let row = format_row(&s, false, false, false, 10_000, 120);
        assert_eq!(row.chars().count(), 120);
    }

    #[test]
    fn format_row_progress_rendered() {
        let s = sum("abc", "feat");
        let row = format_row(&s, false, false, false, 10_000, 80);
        assert!(row.contains("3/10"), "row = {row:?}");
    }

    // --- fuzzy_filter_and_sort ---------------------------------------------

    fn cache_of(active: &[(&str, &str)], resurrect: &[(&str, &str)]) -> PickerCache {
        let mut c = PickerCache::default();
        for (id, name) in active {
            c.active.insert((*id).into(), sum(id, name));
        }
        for (id, name) in resurrect {
            let mut s = sum(id, name);
            s.phase = "crashed".into();
            c.resurrectable.insert((*id).into(), s);
        }
        c
    }

    #[test]
    fn fuzzy_empty_query_returns_all() {
        let c = cache_of(&[("a", "alpha"), ("b", "beta")], &[("c", "crashed-one")]);
        let out = fuzzy_filter_and_sort(&c, "");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn fuzzy_partial_match_filters_subset() {
        let c = cache_of(&[("a", "alpha"), ("b", "beta")], &[]);
        let out = fuzzy_filter_and_sort(&c, "alp");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "a");
    }

    #[test]
    fn fuzzy_is_case_insensitive() {
        let c = cache_of(&[("a", "Alpha")], &[]);
        let out = fuzzy_filter_and_sort(&c, "ALP");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn fuzzy_score_order_descending() {
        // Two agents; query exactly matches one name and fuzzily the
        // other. Exact match should sort first.
        let c = cache_of(&[("a", "alpha"), ("b", "aalpha")], &[]);
        let out = fuzzy_filter_and_sort(&c, "alpha");
        assert_eq!(out.len(), 2);
        assert!(out[0].1 >= out[1].1);
    }

    #[test]
    fn fuzzy_no_match_returns_empty() {
        let c = cache_of(&[("a", "alpha")], &[]);
        let out = fuzzy_filter_and_sort(&c, "zzzzz");
        assert!(out.is_empty());
    }

    // --- T-125: fuzzy ordering + stable tie-break + mixed-case match ------

    #[test]
    fn fuzzy_exact_prefix_substring_fuzzy_descending_order() {
        // T-125 / cavekit-plugin-picker R3: nucleo-matcher scoring must
        // prefer exact/prefix matches over substring/fuzzy matches.
        //
        // Names chosen so the haystack `{orch}:{name} {id}` exhibits a
        // clean gradient when matched against `auth`:
        //   - "auth"        → exact token → highest score
        //   - "authservice" → prefix      → high score
        //   - "myauthsvc"   → substring   → middle score
        //   - "authfooxyz"  → fuzzy       → lower than exact+prefix
        //
        // We assert the exact-match id comes first and that the entire
        // result set is in monotonically non-increasing score order
        // (nucleo's exact score values are an implementation detail).
        let c = cache_of(
            &[
                ("fuzz", "authfooxyz"),
                ("pre", "authservice"),
                ("sub", "myauthsvc"),
                ("exact", "auth"),
            ],
            &[],
        );
        let out = fuzzy_filter_and_sort(&c, "auth");
        assert_eq!(out.len(), 4, "all four agents match fuzzily");
        assert_eq!(out[0].0, "exact", "exact match wins, got {out:?}");
        // Monotonically non-increasing by score.
        for pair in out.windows(2) {
            assert!(
                pair[0].1 >= pair[1].1,
                "scores must be descending: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn fuzzy_stable_tie_break_preserves_cache_order() {
        // T-125 / R3: nucleo scores are deterministic for identical
        // haystacks, so two agents whose names produce the same score
        // must fall back to the cache's BTreeMap iteration order
        // (active first, then resurrectable, each sorted by id). Tests
        // that the sort comparator carries the stable secondary key
        // (original index) — otherwise the picker row order jitters
        // between refreshes for equal-scoring agents.
        let c = cache_of(&[("a", "auth"), ("b", "auth")], &[]);
        let out = fuzzy_filter_and_sort(&c, "auth");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].1, out[1].1, "scores must be equal for tie-break");
        // BTreeMap iterates keys sorted — "a" before "b".
        assert_eq!(
            (out[0].0.as_str(), out[1].0.as_str()),
            ("a", "b"),
            "equal scores must preserve BTreeMap order"
        );
    }

    #[test]
    fn fuzzy_mixed_case_query_matches_mixed_case_name() {
        // T-125 / R3: "case-insensitive" — a query of mixed case must
        // match a name of different mixed case. Needle is lowered
        // before matching; haystack is lowered by nucleo's config. Tests
        // three permutations (upper-needle, mixed-needle, upper-name)
        // collapse to the same match set.
        let c = cache_of(&[("id-1", "CamelCaseName")], &[]);

        for query in ["camel", "CAMEL", "CaMeL"] {
            let out = fuzzy_filter_and_sort(&c, query);
            assert_eq!(
                out.len(),
                1,
                "query {query:?} must match CamelCaseName case-insensitively"
            );
            assert_eq!(out[0].0, "id-1");
        }
    }

    // --- handle_list_key ---------------------------------------------------

    #[test]
    fn key_esc_closes() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "x")], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Esc),
            PickerAction::Close
        );
    }

    #[test]
    fn key_down_then_up_moves_selection() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "x"), ("b", "y")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Down);
        assert_eq!(st.selected, 1);
        handle_list_key(&mut st, &c, KeyInput::Up);
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn key_enter_on_active_opens_session() {
        // T-107 / R8: Enter on an active agent jumps directly to the
        // agent's zellij session. F-601: the action now carries
        // `summary.session` (the real suffixed zellij session id) rather
        // than the bare human name — otherwise `switch_session` targets
        // a session that does not exist.
        let mut st = ListState::default();
        let c = cache_of(&[("a", "x")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(
            action,
            PickerAction::OpenSession("ark-cavekit-x-ABCDEFGH".to_string())
        );
    }

    #[test]
    fn key_enter_on_active_falls_back_to_name_when_session_empty() {
        // F-601: older supervisors (pre-F-600) do not stamp the suffixed
        // session onto spec.json — `summary.session` is empty. In that
        // case we fall back to `summary.name` so the handler keeps
        // working against legacy state dirs rather than silently passing
        // an empty session id to `switch_session`.
        let mut st = ListState::default();
        let mut c = cache_of(&[("a", "x")], &[]);
        c.active.get_mut("a").unwrap().session.clear();
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(action, PickerAction::OpenSession("x".to_string()));
    }

    #[test]
    fn key_enter_on_resurrectable_is_noop() {
        // List-and-attach-only: crashed rows are read-only markers.
        // There is no CLI surface to spawn arbitrary agents, so Enter
        // on a resurrectable is a no-op.
        let mut st = ListState::default();
        let c = cache_of(&[], &[("z", "old")]);
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(action, PickerAction::None);
    }

    #[test]
    fn key_enter_on_done_phase_opens_session() {
        // Agents in terminal phases are still switchable; Enter just
        // jumps to the session so the operator can inspect final state.
        let mut st = ListState::default();
        let mut c = cache_of(&[("a", "x")], &[]);
        c.active.get_mut("a").unwrap().phase = "Done".into();
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(
            action,
            PickerAction::OpenSession("ark-cavekit-x-ABCDEFGH".to_string())
        );
    }

    #[test]
    fn is_terminal_phase_covers_done_failed_killed_timeout() {
        assert!(is_terminal_phase("Done"));
        assert!(is_terminal_phase("Failed"));
        assert!(is_terminal_phase("Killed"));
        assert!(is_terminal_phase("Timeout"));
        assert!(is_terminal_phase("done"));
        assert!(!is_terminal_phase("running"));
        assert!(!is_terminal_phase("stalled"));
    }

    #[test]
    fn key_delete_on_active_confirms_kill() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "x")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Delete);
        assert_eq!(action, PickerAction::ConfirmKill("a".to_string()));
    }

    #[test]
    fn key_delete_on_resurrectable_noop() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[("z", "old")]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Delete),
            PickerAction::None
        );
    }

    #[test]
    fn key_char_appends_to_filter_when_filter_active() {
        let mut st = ListState {
            filter_active: true,
            ..Default::default()
        };
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Char('a'));
        assert_eq!(st.filter, "a");
    }

    #[test]
    fn key_backspace_pops_filter() {
        let mut st = ListState {
            filter: "abc".into(),
            ..Default::default()
        };
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Backspace);
        assert_eq!(st.filter, "ab");
    }

    #[test]
    fn key_question_mark_opens_help() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Char('?')),
            PickerAction::OpenHelp
        );
    }

    #[test]
    fn key_lowercase_r_on_active_is_noop_in_nav_mode() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Char('r'));
        assert_eq!(action, PickerAction::None);
        assert_eq!(st.filter, "");
    }

    #[test]
    fn key_lowercase_r_on_resurrectable_is_noop_in_nav_mode() {
        // List-and-attach-only: `r` no longer resurrects crashed agents.
        let mut st = ListState::default();
        let c = cache_of(&[], &[("z", "old")]);
        let action = handle_list_key(&mut st, &c, KeyInput::Char('r'));
        assert_eq!(action, PickerAction::None);
    }

    #[test]
    fn key_lowercase_r_in_filter_mode_appends_to_filter() {
        let mut st = ListState {
            filter_active: true,
            ..Default::default()
        };
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Char('r'));
        assert_eq!(st.filter, "r");
    }

    // --- T-108: extended R9 keybinding map ---------------------------------

    #[test]
    fn key_j_moves_selection_down() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha"), ("b", "bravo")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::J);
        assert_eq!(action, PickerAction::MoveDown);
        assert_eq!(st.selected, 1);
    }

    #[test]
    fn key_k_moves_selection_up() {
        let mut st = ListState {
            selected: 1,
            ..Default::default()
        };
        let c = cache_of(&[("a", "alpha"), ("b", "bravo")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::K);
        assert_eq!(action, PickerAction::MoveUp);
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn key_l_on_active_expands_detail() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::L);
        assert_eq!(action, PickerAction::ExpandDetail("a".to_string()));
    }

    #[test]
    fn key_h_collapses_detail() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::H);
        assert_eq!(action, PickerAction::CollapseDetail);
    }

    #[test]
    fn key_slash_activates_filter_mode() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Slash);
        assert_eq!(action, PickerAction::FilterChanged);
        assert!(st.filter_active);
    }

    #[test]
    fn key_char_slash_also_activates_filter_mode() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Char('/'));
        assert!(st.filter_active);
    }

    #[test]
    fn filter_mode_then_type_then_esc_exits_filter() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Slash);
        handle_list_key(&mut st, &c, KeyInput::Char('f'));
        handle_list_key(&mut st, &c, KeyInput::Char('o'));
        assert_eq!(st.filter, "fo");
        let action = handle_list_key(&mut st, &c, KeyInput::Esc);
        assert_eq!(action, PickerAction::FilterChanged);
        assert!(!st.filter_active);
        // Filter contents preserved after exiting capture mode.
        assert_eq!(st.filter, "fo");
    }

    #[test]
    fn key_question_opens_help() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Question),
            PickerAction::OpenHelp
        );
    }

    #[test]
    fn key_shift_del_kills_all_done_failed() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::ShiftDel),
            PickerAction::KillAllDoneFailed
        );
    }

    #[test]
    fn key_ctrl_c_closes() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::CtrlC),
            PickerAction::Close
        );
    }

    #[test]
    fn esc_in_nav_mode_closes() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Esc),
            PickerAction::Close
        );
    }

    #[test]
    fn esc_in_filter_mode_only_exits_filter() {
        let mut st = ListState {
            filter_active: true,
            ..Default::default()
        };
        let c = cache_of(&[], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Esc);
        assert_eq!(action, PickerAction::FilterChanged);
        assert!(!st.filter_active);
    }

    // --- header / footer ---------------------------------------------------

    #[test]
    fn header_mentions_counts() {
        let h = build_header(3, 2);
        assert!(h.contains("3 active"));
        assert!(h.contains("2 crashed"));
        assert!(h.contains("?"));
    }

    #[test]
    fn footer_lists_primary_bindings() {
        let f = build_footer();
        assert!(f.contains("Enter"));
        assert!(f.contains("Del"));
        assert!(f.contains("Esc"));
        assert!(f.contains('?'));
    }
}
