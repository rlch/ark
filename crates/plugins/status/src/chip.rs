//! Chip model and pure rendering helpers for the status bar.
//!
//! Satisfies `context/kits/cavekit-plugin-status.md` R3 helpers. The wasm-only
//! `render()` impl lives in [`super::wasm_plugin`]; every decision that can be
//! made without the zellij host (icon selection, severity colour choice,
//! width-aware truncation, focused-session pinning) is extracted here so host
//! tests can exercise it without a wasm runtime.
//!
//! # Design
//!
//! The plugin receives `StatusSummary` entries from supervisors (R2). Render
//! converts each summary into a [`Chip`] whose text has a stable shape:
//!
//! ```text
//! {icon} {orchestrator}:{name} {extra}
//! ```
//!
//! where `extra` is "phase" fallback per R3. Supervisors in this tier don't
//! yet emit progress tuples or findings counts, so `extra` degrades to the
//! phase string; T-097 extends this once the pipe schema grows.
//!
//! Icons/emojis are width-2 in terminals that honour East Asian Wide. We hard
//! code that assumption to avoid pulling `unicode-width` (not a workspace dep
//! in this tier) — see the `BUILD` section of T-096 for the trade-off.
//!
//! # Focused-session pinning
//!
//! `fit_chips` guarantees: if a chip's `name` matches the focused session
//! name passed in, that chip appears in row 1 (never truncated away). Other
//! chips wrap to row 2 when they overflow; overflow past row 2 is dropped.

use super::StatusSummary;

/// Phase inferred from `StatusSummary::phase` + stalled flag.
///
/// Kept separate from the wire-level `phase` string so render code does not
/// match on stringly-typed state. The `Stalled` variant is synthetic — it is
/// set when `stalled_since_secs` would be non-null (future R2 extension);
/// today it is only reached via [`phase_from_str`] when phase == `"stalled"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Agent actively producing output — R3 cyan `⟳`.
    Running,
    /// Agent paused by orchestrator — render as `⏸` yellow.
    Idle,
    /// Waiting on user / prompt — render as `⏸` yellow (visually = idle).
    Prompting,
    /// Blocked / stalled per supervisor heuristics — R3 yellow `⏸`.
    Stalled,
    /// Human review mode — R3 purple `🔍`.
    Reviewing,
    /// Agent completed successfully — R3 green `✓`.
    Done,
    /// Agent exited with non-zero / reported failure — R3 red `✗`.
    Failed,
    /// Agent crashed / host killed it — R3 magenta `💀`.
    Crashed,
    /// Unknown phase string — render as `⚠` so it is visually distinct.
    Unknown,
}

/// Severity bucket — drives `Text::color_range` level selection on wasm.
///
/// Mapping per R3 icon semantics:
///   ok    → done (green)
///   warn  → idle / prompting / stalled (yellow)
///   danger→ failed / crashed (red)
///   info  → running / reviewing (cyan-ish)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Green (`color_range` success level on wasm).
    Ok,
    /// Yellow/orange — user attention required but not fatal.
    Warn,
    /// Red/magenta — fatal / lost progress.
    Danger,
    /// Cyan — informational / in-flight.
    Info,
}

/// A rendered chip — the minimum unit of the status bar.
///
/// `text` is already pre-formatted (`"{icon} {orchestrator}:{name} {extra}"`).
/// Storing the rendered text on the struct lets `fit_chips` reason about
/// width without re-running formatting every iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// Severity leading icon (e.g. `⟳`, `✓`). Stored separately from `text`
    /// to keep render/colour logic from re-parsing the leading char.
    pub icon: char,
    /// Pre-formatted chip text, including icon, name, and phase suffix.
    pub text: String,
    /// Canonical phase enum used by render for colour-range calculation.
    pub phase: Phase,
    /// Severity bucket driving colour choice.
    pub severity: Severity,
}

impl Chip {
    /// Display width in terminal columns.
    ///
    /// We approximate with: emoji / wide chars count as 2 columns, everything
    /// else as 1. This matches what most terminals do for the emoji set we
    /// use (⟳⏸⚠✓✗💀🔍). Pulling `unicode-width` would be more correct but
    /// costs a dep; keep the approximation consistent with
    /// [`char_display_width`].
    pub fn width(&self) -> usize {
        self.text.chars().map(char_display_width).sum()
    }
}

/// Approximate display width of `c`.
///
/// Characters in the Miscellaneous Symbols (U+2600..=U+26FF) and Emoticons
/// (U+1F300..=U+1FAFF) blocks — plus the specific icons from R3 that fall
/// outside those ranges — are treated as width 2. Everything else is width 1,
/// control chars included (they shouldn't appear in chip text).
pub fn char_display_width(c: char) -> usize {
    let cp = c as u32;
    // R3 icons: ⟳ U+27F3, ⏸ U+23F8, ⚠ U+26A0, ✓ U+2713, ✗ U+2717,
    //           💀 U+1F480, 🔍 U+1F50D. Most fall in the wide emoji ranges;
    // the two ASCII-plane chars (✓ ✗) are technically narrow on some
    // terminals but render wide when followed by VS16 — the practical
    // compromise here is to treat every R3 icon as width 2 so layout stays
    // consistent.
    matches!(c, '⟳' | '⏸' | '⚠' | '✓' | '✗' | '🔍' | '💀')
        .then_some(2)
        .unwrap_or_else(|| {
            if (0x1F300..=0x1FAFF).contains(&cp) || (0x2600..=0x26FF).contains(&cp) {
                2
            } else {
                1
            }
        })
}

/// Parse the wire-level phase string into a [`Phase`].
///
/// Unknown / empty strings fall through to `Phase::Unknown` so render still
/// has something to show rather than silently dropping the chip.
pub fn phase_from_str(phase: &str) -> Phase {
    match phase {
        "running" => Phase::Running,
        "idle" => Phase::Idle,
        "prompting" => Phase::Prompting,
        "stalled" => Phase::Stalled,
        "reviewing" => Phase::Reviewing,
        "done" => Phase::Done,
        "failed" => Phase::Failed,
        "crashed" => Phase::Crashed,
        _ => Phase::Unknown,
    }
}

/// Icon for a phase, per R3.
pub fn phase_icon(phase: &str) -> char {
    match phase_from_str(phase) {
        Phase::Running => '⟳',
        Phase::Idle | Phase::Prompting | Phase::Stalled => '⏸',
        Phase::Reviewing => '🔍',
        Phase::Done => '✓',
        Phase::Failed => '✗',
        Phase::Crashed => '💀',
        Phase::Unknown => '⚠',
    }
}

/// Severity bucket for a phase.
pub fn phase_severity(phase: &str) -> Severity {
    match phase_from_str(phase) {
        Phase::Done => Severity::Ok,
        Phase::Failed | Phase::Crashed => Severity::Danger,
        Phase::Running | Phase::Reviewing => Severity::Info,
        Phase::Idle | Phase::Prompting | Phase::Stalled | Phase::Unknown => Severity::Warn,
    }
}

/// Build a chip from a summary.
///
/// `is_focused` is accepted for forward compatibility (future R3 extensions
/// may bold the focused chip); currently unused for formatting but retained
/// so the wasm render path and host tests share one constructor signature.
pub fn build_chip(summary: &StatusSummary, _is_focused: bool) -> Chip {
    let icon = phase_icon(&summary.phase);
    let severity = phase_severity(&summary.phase);
    let phase = phase_from_str(&summary.phase);

    // Compose `{icon} {orchestrator}:{name} {extra}`. Orchestrator can be
    // empty (older payloads) — in that case collapse to just the name.
    let label = if summary.orchestrator.is_empty() {
        summary.name.clone()
    } else {
        format!("{}:{}", summary.orchestrator, summary.name)
    };
    // `extra`: R3 prefers `N/M` progress, falls back to phase text. Only
    // `phase` is on the wire today (R2 scope), so that's what we show.
    let extra = summary.phase.as_str();
    let text = if label.is_empty() {
        format!("{icon} {extra}")
    } else if extra.is_empty() {
        format!("{icon} {label}")
    } else {
        format!("{icon} {label} {extra}")
    };
    Chip {
        icon,
        text,
        phase,
        severity,
    }
}

/// Separator width between chips when rendering (two spaces per R3 example).
pub const CHIP_SEPARATOR_WIDTH: usize = 2;

/// Pack chips into two rows subject to terminal width.
///
/// Returns `(row1, row2)`. Rules:
///   1. Zero-width output (`cols == 0`) yields two empty rows.
///   2. Chips are placed in given order; a chip is pushed to the current row
///      if its width + separator fits the remaining columns, otherwise the
///      next row is started.
///   3. Overflow past row 2 is dropped — except:
///   4. If `focused_session_name` matches a chip's label (found by substring
///      match on the orchestrator:name portion), that chip is *always*
///      placed first on row 1, even if the chip would otherwise have been
///      truncated away. This satisfies R3's "focused session always visible"
///      guarantee.
///
/// The separator is *between* chips, not after the last one, so the width
/// budget accounting subtracts `CHIP_SEPARATOR_WIDTH` only when the row is
/// non-empty.
pub fn fit_chips(
    chips: Vec<Chip>,
    cols: usize,
    focused_session_name: Option<&str>,
) -> (Vec<Chip>, Vec<Chip>) {
    if cols == 0 {
        return (Vec::new(), Vec::new());
    }

    // Extract focused chip (first match wins) so it always lands on row 1.
    let (focused, others) = partition_focused(chips, focused_session_name);

    let mut row1: Vec<Chip> = Vec::new();
    let mut row2: Vec<Chip> = Vec::new();
    let mut row1_used = 0usize;
    let mut row2_used = 0usize;

    // Pin focused chip first on row 1 if it fits alone.
    if let Some(fchip) = focused {
        let w = fchip.width();
        if w <= cols {
            row1_used = w;
            row1.push(fchip);
        }
        // If the focused chip itself exceeds cols we drop it — row would
        // not be renderable anyway. This is the only degenerate path that
        // violates the "always visible" promise, and only at absurd widths.
    }

    for chip in others {
        let w = chip.width();
        if w > cols {
            // Single chip too wide — skip entirely (truncation of an
            // individual chip is T-097's concern).
            continue;
        }
        let row1_cost = if row1.is_empty() {
            w
        } else {
            w + CHIP_SEPARATOR_WIDTH
        };
        if row1_used + row1_cost <= cols {
            row1_used += row1_cost;
            row1.push(chip);
            continue;
        }
        let row2_cost = if row2.is_empty() {
            w
        } else {
            w + CHIP_SEPARATOR_WIDTH
        };
        if row2_used + row2_cost <= cols {
            row2_used += row2_cost;
            row2.push(chip);
            continue;
        }
        // Overflow past two rows: drop.
    }

    (row1, row2)
}

/// Split the chip list into `(focused, rest)`.
///
/// Matching is done against `"{orchestrator}:{name}"` first, then bare
/// `"{name}"`, because supervisors may register sessions under either form.
/// Match is case-sensitive and exact on the substring boundary — no fuzzy
/// matching, to avoid surprise pins.
fn partition_focused(
    chips: Vec<Chip>,
    focused_session_name: Option<&str>,
) -> (Option<Chip>, Vec<Chip>) {
    let Some(name) = focused_session_name else {
        return (None, chips);
    };
    let mut focused: Option<Chip> = None;
    let mut rest: Vec<Chip> = Vec::with_capacity(chips.len());
    for chip in chips {
        if focused.is_none() && chip_matches_session(&chip, name) {
            focused = Some(chip);
        } else {
            rest.push(chip);
        }
    }
    (focused, rest)
}

/// True when `chip.text` matches the focused zellij session.
///
/// # F-602
///
/// After F-522 the real zellij session name is
/// `ark-{orchestrator}-{name}-{ulid8}` — the picker and supervisor now both
/// emit sessions in that shape. `focused_session_name` passed in from the
/// zellij host therefore is the FULL suffixed identifier, not the bare
/// `name` the chip renders.
///
/// We match in three complementary ways so both the legacy bare-name and
/// the new suffixed form resolve to the same chip:
///
/// 1. Exact-token equality against every space-split token in `chip.text`
///    (covers `orch:name` tokens and the bare name when the chip label
///    happens to include it).
/// 2. `orch:name` token equality, comparing just the `name` half.
/// 3. If the focused session looks like `ark-{orch}-{name}-{ulid8}`,
///    strip the `ark-` prefix and the trailing `-{ulid8}` (8-char
///    Crockford-base32 suffix) and retry step 1/2 against the recovered
///    bare name.
fn chip_matches_session(chip: &Chip, session_name: &str) -> bool {
    if chip_label_contains(chip, session_name) {
        return true;
    }
    if let Some(bare) = extract_bare_name_from_session(session_name)
        && chip_label_contains(chip, bare)
    {
        return true;
    }
    false
}

/// Step (1)+(2) above: exact token match or `orch:name` tail match.
fn chip_label_contains(chip: &Chip, needle: &str) -> bool {
    for token in chip.text.split_whitespace() {
        if token == needle {
            return true;
        }
        if let Some((_orch, name)) = token.split_once(':')
            && name == needle
        {
            return true;
        }
    }
    false
}

/// Recover the bare agent name from a full zellij session identifier of
/// the shape `ark-{orch}-{name}-{ulid8}`. Returns `None` if the input
/// does not match that shape (e.g. legacy bare-name sessions, or any
/// non-ark session the zellij host reports).
///
/// The 8-char suffix must be Crockford-base32 uppercase — the format
/// [`ark-cli::commands::spawn::unique_session_name`] emits — so an
/// unrelated session name that merely contains hyphens does not get
/// mis-parsed.
fn extract_bare_name_from_session(session: &str) -> Option<&str> {
    let body = session.strip_prefix("ark-")?;
    // Split into `orch-name-suffix`. We need the last hyphen to peel the
    // ulid8 suffix, and the first hyphen to drop the orchestrator slug.
    let (head, suffix) = body.rsplit_once('-')?;
    if suffix.len() != 8
        || !suffix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let (_orch, name) = head.split_once('-')?;
    if name.is_empty() {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, phase: &str) -> StatusSummary {
        StatusSummary {
            agent_id: name.to_string(),
            name: name.to_string(),
            orchestrator: "cavekit".to_string(),
            phase: phase.to_string(),
            updated_at: 0,
            last_event: String::new(),
        }
    }

    #[test]
    fn phase_icon_covers_all_r3_icons() {
        assert_eq!(phase_icon("running"), '⟳');
        assert_eq!(phase_icon("stalled"), '⏸');
        assert_eq!(phase_icon("idle"), '⏸');
        assert_eq!(phase_icon("reviewing"), '🔍');
        assert_eq!(phase_icon("done"), '✓');
        assert_eq!(phase_icon("failed"), '✗');
        assert_eq!(phase_icon("crashed"), '💀');
        // Unknown / empty falls through to the warning glyph.
        assert_eq!(phase_icon("nonsense"), '⚠');
    }

    #[test]
    fn phase_severity_maps_each_bucket() {
        assert_eq!(phase_severity("done"), Severity::Ok);
        assert_eq!(phase_severity("failed"), Severity::Danger);
        assert_eq!(phase_severity("crashed"), Severity::Danger);
        assert_eq!(phase_severity("running"), Severity::Info);
        assert_eq!(phase_severity("reviewing"), Severity::Info);
        assert_eq!(phase_severity("idle"), Severity::Warn);
        assert_eq!(phase_severity("prompting"), Severity::Warn);
        assert_eq!(phase_severity("stalled"), Severity::Warn);
        assert_eq!(phase_severity("bogus"), Severity::Warn);
    }

    #[test]
    fn build_chip_produces_expected_shape() {
        let chip = build_chip(&summary("auth", "running"), false);
        assert_eq!(chip.icon, '⟳');
        assert_eq!(chip.phase, Phase::Running);
        assert_eq!(chip.severity, Severity::Info);
        assert!(chip.text.contains("cavekit:auth"));
        assert!(chip.text.starts_with('⟳'));
    }

    #[test]
    fn build_chip_without_orchestrator_collapses_label() {
        let mut s = summary("solo", "done");
        s.orchestrator.clear();
        let chip = build_chip(&s, false);
        assert!(!chip.text.contains(':'));
        assert!(chip.text.contains("solo"));
        assert_eq!(chip.severity, Severity::Ok);
    }

    #[test]
    fn fit_chips_single_row_leaves_row2_empty() {
        let chips = vec![
            build_chip(&summary("a", "running"), false),
            build_chip(&summary("b", "done"), false),
        ];
        let (row1, row2) = fit_chips(chips, 200, None);
        assert_eq!(row1.len(), 2);
        assert!(row2.is_empty());
    }

    #[test]
    fn fit_chips_overflows_to_row2() {
        // Many chips, narrow width → forces wrapping.
        let chips: Vec<Chip> = (0..6)
            .map(|i| build_chip(&summary(&format!("agent{i}"), "running"), false))
            .collect();
        let total_w: usize = chips.iter().map(|c| c.width()).sum::<usize>()
            + CHIP_SEPARATOR_WIDTH * (chips.len() - 1);
        // Pick a width that fits roughly half the chips per row.
        let cols = total_w / 2 + 2;
        let (row1, row2) = fit_chips(chips, cols, None);
        assert!(!row1.is_empty());
        assert!(!row2.is_empty());
    }

    #[test]
    fn fit_chips_truncates_past_two_rows() {
        // Width enough for exactly one chip per row → 3rd+ chips get dropped.
        let chips: Vec<Chip> = (0..5)
            .map(|i| build_chip(&summary(&format!("agent{i}"), "running"), false))
            .collect();
        let one_chip_w = chips[0].width();
        let (row1, row2) = fit_chips(chips, one_chip_w, None);
        assert_eq!(row1.len(), 1);
        assert_eq!(row2.len(), 1);
    }

    #[test]
    fn fit_chips_keeps_focused_chip_in_row1_even_under_truncation() {
        // Build 6 chips; force one-per-row widths; focused chip is last in
        // the input list so without pinning it would be dropped.
        let chips: Vec<Chip> = (0..6)
            .map(|i| build_chip(&summary(&format!("agent{i}"), "running"), false))
            .collect();
        let one_chip_w = chips[0].width();
        let (row1, row2) = fit_chips(chips, one_chip_w, Some("agent5"));
        assert_eq!(row1.len(), 1);
        assert!(
            row1[0].text.contains("agent5"),
            "focused chip must land on row 1, got {:?}",
            row1[0].text
        );
        // agent0..agent4 compete for row2; only one fits.
        assert_eq!(row2.len(), 1);
    }

    #[test]
    fn fit_chips_focused_always_first_on_row1() {
        let chips: Vec<Chip> = ["a", "b", "c"]
            .iter()
            .map(|n| build_chip(&summary(n, "running"), false))
            .collect();
        let (row1, _row2) = fit_chips(chips, 200, Some("b"));
        assert!(row1[0].text.contains("cavekit:b"));
    }

    #[test]
    fn fit_chips_zero_cols_yields_empty_output() {
        let chips = vec![build_chip(&summary("a", "running"), false)];
        let (row1, row2) = fit_chips(chips, 0, None);
        assert!(row1.is_empty());
        assert!(row2.is_empty());
    }

    #[test]
    fn fit_chips_drops_chip_that_exceeds_cols_alone() {
        let big = build_chip(&summary("verylongagentname", "running"), false);
        let big_w = big.width();
        let small = build_chip(&summary("a", "done"), false);
        let (row1, row2) = fit_chips(vec![big, small.clone()], big_w - 1, None);
        // Big chip dropped (too wide), small one fits row1.
        assert_eq!(row1.len(), 1);
        assert_eq!(row1[0].text, small.text);
        assert!(row2.is_empty());
    }

    // --- F-602: focused session names carry the F-522 ULID suffix -------

    #[test]
    fn chip_matches_session_bare_name_still_works() {
        // Legacy shape — pre-F-522 sessions were the bare `name`. The
        // chip label is `cavekit:auth`, so matching against "auth"
        // resolves via the `orch:name` tail branch.
        let chip = build_chip(&summary("auth", "running"), false);
        assert!(chip_matches_session(&chip, "auth"));
    }

    #[test]
    fn chip_matches_session_orchestrator_prefixed() {
        // `cavekit:auth` token equality — matches when the focused
        // session string IS the rendered `orch:name` token.
        let chip = build_chip(&summary("auth", "running"), false);
        assert!(chip_matches_session(&chip, "cavekit:auth"));
    }

    #[test]
    fn chip_matches_session_full_zellij_name_after_f522() {
        // F-602: the zellij host now reports focused session as the full
        // `ark-{orch}-{name}-{ulid8}` form. The chip's label still only
        // contains `cavekit:auth`, so matching must peel the ark- prefix
        // and the 8-char ULID suffix to recover the bare name.
        let chip = build_chip(&summary("auth", "running"), false);
        assert!(chip_matches_session(&chip, "ark-cavekit-auth-01ABCDEF"));
    }

    #[test]
    fn chip_matches_session_rejects_short_suffix() {
        // Defensive: a 7-char trailing segment is NOT a ULID fragment,
        // so we must not silently peel it and false-match against a
        // similarly-named chip. The focused session then fails to pin.
        let chip = build_chip(&summary("auth", "running"), false);
        assert!(!chip_matches_session(&chip, "ark-cavekit-auth-1234567"));
    }

    #[test]
    fn chip_matches_session_rejects_lowercase_suffix() {
        // ULIDs encode as UPPERCASE Crockford-base32; a lowercase tail
        // is almost certainly an unrelated session and must not match.
        let chip = build_chip(&summary("auth", "running"), false);
        assert!(!chip_matches_session(&chip, "ark-cavekit-auth-deadbeef"));
    }

    #[test]
    fn fit_chips_pins_focused_for_suffixed_session() {
        // Wire-through: `fit_chips` with the full suffixed form still
        // pins the right chip to row 1, mirroring the bare-name case.
        let chips: Vec<Chip> = ["a", "b", "c"]
            .iter()
            .map(|n| build_chip(&summary(n, "running"), false))
            .collect();
        let (row1, _row2) = fit_chips(chips, 200, Some("ark-cavekit-b-01ABCDEF"));
        assert!(row1[0].text.contains("cavekit:b"));
    }

    #[test]
    fn extract_bare_name_shapes() {
        assert_eq!(
            extract_bare_name_from_session("ark-cavekit-auth-01ABCDEF"),
            Some("auth")
        );
        // Multi-hyphen name survives — we only peel one trailing ulid8.
        assert_eq!(
            extract_bare_name_from_session("ark-cavekit-multi-word-name-01ABCDEF"),
            Some("multi-word-name")
        );
        assert_eq!(extract_bare_name_from_session("not-an-ark-session"), None);
        assert_eq!(extract_bare_name_from_session("ark-cavekit-auth"), None);
    }
}
