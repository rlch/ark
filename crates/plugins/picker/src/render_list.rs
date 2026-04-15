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
    /// User pressed `r` on a resurrectable agent; exec `ark spawn` with
    /// the archived spec.
    Resurrect(String),
    /// `Ctrl+N` or `N`: open the new-agent form.
    NewAgent,
    /// `?`: open the help overlay.
    OpenHelp,
    /// Filter text changed (append/backspace). UI redraws; no side effect.
    FilterChanged,
    /// Selection moved up.
    MoveUp,
    /// Selection moved down.
    MoveDown,
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
    /// `Ctrl+N` — open new-agent form. (`N` by itself also maps to this.)
    CtrlN,
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
    "Enter: open │ N: new │ Del: kill │ r: resurrect │ ?: help │ Esc: close".to_string()
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
    let mut idx: usize = 0;
    for summary in cache.active.values().chain(cache.resurrectable.values()) {
        let hay_str = fuzzy_haystack(summary);
        let mut hay_buf: Vec<char> = Vec::new();
        let hay = Utf32Str::new(&hay_str, &mut hay_buf);
        if let Some(score) = matcher.fuzzy_match(hay, needle) {
            // score is u16; keep as i32 for the public API so future
            // negative-scoring variants (e.g. demoting dim agents)
            // aren't blocked by the type.
            scored.push((summary.id.clone(), score as i32, idx));
        }
        idx += 1;
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
    match key {
        KeyInput::Esc => PickerAction::Close,
        KeyInput::Up => {
            crate::state::move_selection_up(state, total);
            PickerAction::MoveUp
        }
        KeyInput::Down => {
            crate::state::move_selection_down(state, total);
            PickerAction::MoveDown
        }
        KeyInput::Enter => match selected_agent(cache, state) {
            Some((id, false)) => PickerAction::OpenSession(id.to_string()),
            Some((id, true)) => PickerAction::Resurrect(id.to_string()),
            None => PickerAction::None,
        },
        KeyInput::Delete => match selected_agent(cache, state) {
            Some((id, false)) => PickerAction::ConfirmKill(id.to_string()),
            // Del on a resurrectable is a no-op; the kit reserves `r`
            // for that flow. Returning None keeps the handler total.
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
        KeyInput::CtrlN => PickerAction::NewAgent,
        KeyInput::Char(c) => {
            // R9 bindings: `?` → help, `r` → resurrect (on selected
            // crashed agent), `N` → new (also covered by CtrlN).
            match c {
                '?' => PickerAction::OpenHelp,
                'N' => PickerAction::NewAgent,
                'r' => match selected_agent(cache, state) {
                    Some((id, true)) => PickerAction::Resurrect(id.to_string()),
                    // `r` on a non-crashed agent falls through to the
                    // filter — users might be typing a name that
                    // contains `r`.
                    _ => append_to_filter(state, 'r'),
                },
                c if c.is_control() => PickerAction::None,
                c => append_to_filter(state, c),
            }
        }
        KeyInput::Other => PickerAction::None,
    }
}

/// Append a char to the filter and reset selection — shared between the
/// generic-char branch and the `r`-fallback branch above.
fn append_to_filter(state: &mut ListState, c: char) -> PickerAction {
    state.filter.push(c);
    state.selected = 0;
    PickerAction::FilterChanged
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
    fn key_enter_opens_selected_session() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "x")], &[]);
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(action, PickerAction::OpenSession("a".to_string()));
    }

    #[test]
    fn key_enter_on_resurrectable_triggers_resurrect() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[("z", "old")]);
        let action = handle_list_key(&mut st, &c, KeyInput::Enter);
        assert_eq!(action, PickerAction::Resurrect("z".to_string()));
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
    fn key_char_appends_to_filter() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Char('a'));
        assert_eq!(st.filter, "a");
    }

    #[test]
    fn key_backspace_pops_filter() {
        let mut st = ListState {
            filter: "abc".into(),
            selected: 0,
            scroll_offset: 0,
        };
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Backspace);
        assert_eq!(st.filter, "ab");
    }

    #[test]
    fn key_ctrl_n_emits_new_agent() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::CtrlN),
            PickerAction::NewAgent
        );
    }

    #[test]
    fn key_uppercase_n_emits_new_agent() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[]);
        assert_eq!(
            handle_list_key(&mut st, &c, KeyInput::Char('N')),
            PickerAction::NewAgent
        );
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
    fn key_lowercase_r_on_resurrectable_resurrects() {
        let mut st = ListState::default();
        let c = cache_of(&[], &[("z", "old")]);
        let action = handle_list_key(&mut st, &c, KeyInput::Char('r'));
        assert_eq!(action, PickerAction::Resurrect("z".to_string()));
    }

    #[test]
    fn key_lowercase_r_on_active_falls_through_to_filter() {
        let mut st = ListState::default();
        let c = cache_of(&[("a", "alpha")], &[]);
        handle_list_key(&mut st, &c, KeyInput::Char('r'));
        assert_eq!(st.filter, "r");
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
