//! Picker help overlay rendering (cavekit-plugin-picker R9 / W5).
//!
//! Pure Rust — produces a Vec<String> that the wasm render path feeds
//! into `print_text_with_coordinates`. Keeping this host-testable lets
//! us assert substring coverage of every R9 binding without booting a
//! zellij plugin host.
//!
//! The overlay is opened with `?` (and closed with `?` or `Esc`); see
//! `lib.rs`'s wasm `update` path for the transition handling. The
//! content here intentionally enumerates every R9 binding — if a new
//! key is added to [`crate::KeyInput`] it should also land here so the
//! help screen stays a source of truth.
//!
//! Layout strategy: two-column `key  description` pairs printed in the
//! order R9 lists them. Width-aware: rows pad to `cols` so the caller
//! can apply a full-row highlight if needed without re-measuring.

/// Every R9 binding as a `(key, description)` pair. Kept as a const
/// slice so the render function, tests, and any future "did you mean?"
/// affordance can share one source of truth.
pub const HELP_BINDINGS: &[(&str, &str)] = &[
    ("↑ / k", "move selection up"),
    ("↓ / j", "move selection down"),
    ("→ / l", "expand selected / enter Detail"),
    ("← / h", "collapse Detail"),
    ("Enter", "switch session / confirm"),
    ("/", "focus filter (type to filter)"),
    ("Backspace", "edit filter"),
    ("Ctrl+r", "rename selected agent"),
    ("Ctrl+d", "detach (Forget) selected agent"),
    ("Del", "kill selected agent"),
    ("Shift+Del", "kill all done/failed agents in view"),
    ("Tab", "cycle status filter preset"),
    ("?", "toggle this help overlay"),
    ("Esc / Ctrl+c", "close picker (or exit filter mode)"),
];

/// Header line shown at the top of the help overlay.
pub const HELP_TITLE: &str = "ark picker — keybindings";

/// Footer hint shown at the bottom of the help overlay.
pub const HELP_FOOTER: &str = "Press ? or Esc to close";

/// Render the help overlay as a `Vec<String>` sized for `cols` columns.
///
/// Output shape:
/// - Row 0: [`HELP_TITLE`]
/// - Row 1: blank
/// - Rows 2..N: `{key: <key width>}  {description}` pairs from
///   [`HELP_BINDINGS`]
/// - Row N+1: blank
/// - Row N+2: [`HELP_FOOTER`]
///
/// When `cols` is small the description column truncates via
/// `String::truncate` on the composed line — callers can still draw
/// the overlay in narrow terminals without blowing the width budget.
pub fn render_help_screen(cols: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(HELP_BINDINGS.len() + 4);
    out.push(clip_to_cols(HELP_TITLE.to_string(), cols));
    out.push(String::new());

    let key_col_width = HELP_BINDINGS
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);

    for (key, desc) in HELP_BINDINGS {
        let padded_key = pad_right(key, key_col_width);
        let line = format!("  {padded_key}   {desc}");
        out.push(clip_to_cols(line, cols));
    }

    out.push(String::new());
    out.push(clip_to_cols(HELP_FOOTER.to_string(), cols));
    out
}

/// Truncate `s` to at most `cols` chars. `cols == 0` yields the empty
/// string. Character-aware (not byte-aware) so multi-byte glyphs like
/// `↑` don't produce invalid UTF-8 boundaries.
fn clip_to_cols(s: String, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    if s.chars().count() <= cols {
        return s;
    }
    s.chars().take(cols).collect()
}

/// Right-pad `s` with spaces to `width` characters. No-op when
/// `s.chars().count() >= width`.
fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    let mut out = String::from(s);
    for _ in 0..(width - len) {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_help_screen_non_empty() {
        let rows = render_help_screen(80);
        assert!(!rows.is_empty());
    }

    #[test]
    fn render_help_screen_contains_title_and_footer() {
        let rows = render_help_screen(80);
        let blob = rows.join("\n");
        assert!(blob.contains(HELP_TITLE), "title missing: {blob}");
        assert!(blob.contains(HELP_FOOTER), "footer missing: {blob}");
    }

    #[test]
    fn render_help_screen_mentions_every_r9_binding() {
        let rows = render_help_screen(120);
        let blob = rows.join("\n");
        // Every key label from HELP_BINDINGS should survive into the
        // rendered output — catches regressions when someone edits the
        // const slice but not the renderer.
        for (key, _) in HELP_BINDINGS {
            assert!(blob.contains(key), "missing key binding {key:?}: {blob}");
        }
        // Spot-check the R9 phrases individually in case the label
        // strings change cosmetically.
        for needle in [
            "Shift+Del",
            "Ctrl+r",
            "Ctrl+d",
            "Ctrl+c",
            "?",
            "/",
            "Esc",
            "Enter",
            "Backspace",
            "Tab",
        ] {
            assert!(blob.contains(needle), "missing binding {needle}: {blob}");
        }
    }

    #[test]
    fn render_help_screen_respects_zero_cols() {
        let rows = render_help_screen(0);
        // Structural rows still present; contents clip to empty.
        assert!(!rows.is_empty());
        for r in &rows {
            assert_eq!(r.chars().count(), 0);
        }
    }

    #[test]
    fn render_help_screen_clips_to_cols() {
        let rows = render_help_screen(10);
        for r in &rows {
            assert!(r.chars().count() <= 10, "row too wide: {r:?}");
        }
    }
}
