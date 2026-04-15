//! Picker new-agent form rendering + key handling (cavekit-plugin-picker R6 / W3).
//!
//! Pure, host-testable helpers for the W3 "spawn a new agent" form. Every
//! function here compiles on the host so the state transitions can be
//! exhaustively unit-tested without a wasm target — the wasm plugin in
//! [`crate`] translates key events into [`KeyInput`], calls
//! [`handle_new_agent_key`], and funnels the returned [`PickerAction`] into
//! zellij-tile side effects (mainly `run_command` for the spawn).
//!
//! # Acceptance criteria mapping (`cavekit-plugin-picker.md` R6)
//!
//! - Orchestrator radio (`Cavekit` / `Claude Code`): rendered by
//!   [`build_new_agent_rows`], cycled via `Left`/`Right`.
//! - Cwd text input + `Ctrl+F` filepicker overlay hook: [`apply_char`] /
//!   [`apply_backspace`] for typing; the `Ctrl+F` path sets
//!   `NewAgentState::open_filepicker = true` (STUB — real filepicker is
//!   wired in a later task).
//! - Name defaults to `basename(cwd)`: [`basename_of`] — used by
//!   [`NewAgentState::with_cwd`] as the form opens.
//! - Layout dropdown cycling through `available_layouts`: `Left`/`Right`
//!   while focused on [`FormField::Layout`].
//! - Cmd default `claude --resume`: set in
//!   [`crate::state::NewAgentState::default`].
//! - `Tab`/`Shift+Tab` focus cycle: [`next_field`] / [`prev_field`].
//! - Enter → `ark spawn --orchestrator <o> --cwd <c> --name <n> --layout <l>
//!   -- <cmd>`: [`build_spawn_argv`] produces the argv; the wasm layer
//!   feeds it to `run_command`.
//! - Esc → cancel back to list: [`handle_new_agent_key`] returns
//!   [`PickerAction::CancelNewAgent`].
//!
//! Shlex is used to tokenise the `cmd` field so users can paste
//! `bash -lc "cargo run -- --flag"`-style commands; the dependency is
//! already present in the workspace via `crates/cli`.

use crate::render_list::{KeyInput, PickerAction};
use crate::state::{FormField, NewAgentState, Orchestrator};

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Return the final path segment of `path`, stripping trailing slashes.
///
/// - `"/a/b/c"` → `"c"`
/// - `"/a/b/c/"` → `"c"`
/// - `"/"` → `""`
/// - `""` → `""`
/// - `"a"` → `"a"`
///
/// This is a byte-level string walk rather than `std::path::Path::file_name`
/// because the cwd we're handed is supervisor-recorded text that may not
/// round-trip through `Path` cleanly on every platform. Keeping it a pure
/// string helper also keeps the tests OS-agnostic.
pub fn basename_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind('/') {
        Some(idx) => trimmed[idx + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Cycle focus to the next field (Tab).
///
/// Wraps Submit → Orchestrator so Tab never gets stuck on the button.
pub fn next_field(f: FormField) -> FormField {
    match f {
        FormField::Orchestrator => FormField::Cwd,
        FormField::Cwd => FormField::Name,
        FormField::Name => FormField::Layout,
        FormField::Layout => FormField::Cmd,
        FormField::Cmd => FormField::Submit,
        FormField::Submit => FormField::Orchestrator,
    }
}

/// Cycle focus to the previous field (Shift+Tab). Inverse of
/// [`next_field`].
pub fn prev_field(f: FormField) -> FormField {
    match f {
        FormField::Orchestrator => FormField::Submit,
        FormField::Cwd => FormField::Orchestrator,
        FormField::Name => FormField::Cwd,
        FormField::Layout => FormField::Name,
        FormField::Cmd => FormField::Layout,
        FormField::Submit => FormField::Cmd,
    }
}

/// Insert `c` into the currently focused text field. No-op for the
/// orchestrator radio / layout dropdown / submit button.
pub fn apply_char(state: &mut NewAgentState, c: char) {
    if c.is_control() {
        return;
    }
    match state.focus {
        FormField::Cwd => state.cwd.push(c),
        FormField::Name => state.name.push(c),
        FormField::Cmd => state.cmd.push(c),
        // Orchestrator / Layout / Submit don't accept raw characters.
        FormField::Orchestrator | FormField::Layout | FormField::Submit => {}
    }
}

/// Pop the last char from the currently focused text field.
pub fn apply_backspace(state: &mut NewAgentState) {
    match state.focus {
        FormField::Cwd => {
            state.cwd.pop();
        }
        FormField::Name => {
            state.name.pop();
        }
        FormField::Cmd => {
            state.cmd.pop();
        }
        FormField::Orchestrator | FormField::Layout | FormField::Submit => {}
    }
}

/// Cycle the value of the focused radio/dropdown in the given direction.
///
/// `dir == Direction::Next` moves forward; `Direction::Prev` moves back.
/// No-op for non-cyclable fields (text inputs, Submit).
pub fn cycle_value(state: &mut NewAgentState, dir: Direction) {
    match state.focus {
        FormField::Orchestrator => {
            state.orchestrator = match state.orchestrator {
                Orchestrator::Cavekit => Orchestrator::ClaudeCode,
                Orchestrator::ClaudeCode => Orchestrator::Cavekit,
            };
        }
        FormField::Layout => cycle_layout(state, dir),
        _ => {}
    }
}

/// Direction argument for [`cycle_value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Right-arrow: forward through the list.
    Next,
    /// Left-arrow: backward through the list.
    Prev,
}

fn cycle_layout(state: &mut NewAgentState, dir: Direction) {
    let layouts = &state.available_layouts;
    if layouts.is_empty() {
        return;
    }
    let cur_idx = layouts.iter().position(|l| l == &state.layout).unwrap_or(0);
    let next_idx = match dir {
        Direction::Next => (cur_idx + 1) % layouts.len(),
        Direction::Prev => (cur_idx + layouts.len() - 1) % layouts.len(),
    };
    state.layout = layouts[next_idx].clone();
}

/// Build the argv for `ark spawn` from `state`.
///
/// Shape: `["ark", "spawn", "--orchestrator", "<o>", "--cwd", "<c>",
/// "--name", "<n>", "--layout", "<l>", "--", <cmd tokens...>]`.
///
/// The `cmd` field is tokenised with `shlex` so users can paste quoted
/// arguments; if `shlex::split` fails (unbalanced quotes), we fall back
/// to a naive whitespace split so the button still does *something*.
pub fn build_spawn_argv(state: &NewAgentState) -> Vec<String> {
    let orch = match state.orchestrator {
        Orchestrator::Cavekit => "cavekit",
        Orchestrator::ClaudeCode => "claude-code",
    };
    let mut argv = vec![
        "ark".to_string(),
        "spawn".to_string(),
        "--orchestrator".to_string(),
        orch.to_string(),
        "--cwd".to_string(),
        state.cwd.clone(),
        "--name".to_string(),
        state.name.clone(),
        "--layout".to_string(),
        state.layout.clone(),
        "--".to_string(),
    ];
    let tokens = shlex::split(&state.cmd).unwrap_or_else(|| {
        state
            .cmd
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    });
    argv.extend(tokens);
    argv
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the new-agent form into a flat `Vec<String>` — the wasm layer
/// draws each returned string on consecutive rows.
///
/// The caller supplies the raw state; we format radio markers, focus
/// indicators, and the layout cycler (`< builder >`). Keeping this pure
/// lets snapshot-style tests lock the string output down.
pub fn build_new_agent_rows(state: &NewAgentState) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    rows.push("New Agent".to_string());
    rows.push(String::new());

    // Orchestrator radio.
    let cavekit_marker = if matches!(state.orchestrator, Orchestrator::Cavekit) {
        "•"
    } else {
        " "
    };
    let claude_marker = if matches!(state.orchestrator, Orchestrator::ClaudeCode) {
        "•"
    } else {
        " "
    };
    let orch_focus = if state.focus == FormField::Orchestrator {
        "▸ "
    } else {
        "  "
    };
    rows.push(format!(
        "{orch_focus}Orchestrator: [{cavekit_marker}] Cavekit   [{claude_marker}] Claude Code"
    ));

    // Cwd input.
    rows.push(format_input_row(
        state.focus == FormField::Cwd,
        "Cwd",
        &state.cwd,
        Some("Ctrl+F: filepicker"),
    ));

    // Name input.
    rows.push(format_input_row(
        state.focus == FormField::Name,
        "Name",
        &state.name,
        None,
    ));

    // Layout dropdown.
    let layout_focus = if state.focus == FormField::Layout {
        "▸ "
    } else {
        "  "
    };
    rows.push(format!("{layout_focus}Layout:       < {} >", state.layout));

    // Cmd input.
    rows.push(format_input_row(
        state.focus == FormField::Cmd,
        "Cmd",
        &state.cmd,
        None,
    ));

    // Submit button.
    let submit_focus = if state.focus == FormField::Submit {
        "▸ "
    } else {
        "  "
    };
    rows.push(format!("{submit_focus}[ Spawn ]"));

    rows.push(String::new());
    rows.push("Tab: next │ Shift+Tab: prev │ Enter: spawn │ Esc: cancel".to_string());
    rows
}

fn format_input_row(focused: bool, label: &str, value: &str, hint: Option<&str>) -> String {
    let focus_marker = if focused { "▸ " } else { "  " };
    let cursor = if focused { "_" } else { "" };
    let label_col = format!("{label}:");
    let hint_suffix = match hint {
        Some(h) => format!("   ({h})"),
        None => String::new(),
    };
    // Pad label column to 13 chars to align with the layout/orch rows.
    format!("{focus_marker}{label_col:<13} {value}{cursor}{hint_suffix}")
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Pure key handler for the new-agent form.
///
/// Mutates `state` in response to keys and returns the action the wasm
/// layer should fire. Enter on any field means "submit" — users don't
/// need to tab to the button first. Esc cancels back to the list.
pub fn handle_new_agent_key(state: &mut NewAgentState, key: KeyInput) -> PickerAction {
    match key {
        KeyInput::Esc => PickerAction::CancelNewAgent,
        KeyInput::Enter => PickerAction::SubmitNewAgent,
        KeyInput::Tab => {
            state.focus = next_field(state.focus);
            PickerAction::None
        }
        KeyInput::ShiftTab => {
            state.focus = prev_field(state.focus);
            PickerAction::None
        }
        KeyInput::Backspace => {
            apply_backspace(state);
            PickerAction::None
        }
        KeyInput::Left => {
            cycle_value(state, Direction::Prev);
            PickerAction::None
        }
        KeyInput::Right => {
            cycle_value(state, Direction::Next);
            PickerAction::None
        }
        KeyInput::CtrlF => {
            if state.focus == FormField::Cwd {
                state.open_filepicker = true;
            }
            PickerAction::None
        }
        KeyInput::Char(c) => {
            apply_char(state, c);
            PickerAction::None
        }
        // Up/Down/Delete/CtrlN/CtrlR/CtrlD not bound on this screen.
        KeyInput::Up
        | KeyInput::Down
        | KeyInput::Delete
        | KeyInput::CtrlN
        | KeyInput::CtrlR
        | KeyInput::CtrlD
        | KeyInput::Other => PickerAction::None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- basename_of ---------------------------------------------------------

    #[test]
    fn basename_of_absolute_path() {
        assert_eq!(basename_of("/a/b/c"), "c");
    }

    #[test]
    fn basename_of_trailing_slash() {
        assert_eq!(basename_of("/a/b/c/"), "c");
    }

    #[test]
    fn basename_of_root() {
        assert_eq!(basename_of("/"), "");
    }

    #[test]
    fn basename_of_empty() {
        assert_eq!(basename_of(""), "");
    }

    #[test]
    fn basename_of_single_segment() {
        assert_eq!(basename_of("a"), "a");
    }

    // --- next_field / prev_field --------------------------------------------

    #[test]
    fn next_field_cycles_through_all_variants() {
        let mut f = FormField::Orchestrator;
        for _ in 0..6 {
            f = next_field(f);
        }
        // Six Tab presses from Orchestrator → back to Orchestrator.
        assert_eq!(f, FormField::Orchestrator);
    }

    #[test]
    fn prev_field_is_inverse_of_next_field() {
        for start in [
            FormField::Orchestrator,
            FormField::Cwd,
            FormField::Name,
            FormField::Layout,
            FormField::Cmd,
            FormField::Submit,
        ] {
            assert_eq!(prev_field(next_field(start)), start);
        }
    }

    // --- apply_char / apply_backspace ---------------------------------------

    #[test]
    fn apply_char_inserts_into_focused_text_field() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Name;
        apply_char(&mut s, 'x');
        apply_char(&mut s, 'y');
        assert_eq!(s.name, "xy");
    }

    #[test]
    fn apply_char_ignores_when_focus_is_orchestrator() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Orchestrator;
        apply_char(&mut s, 'x');
        assert_eq!(s.cwd, "");
        assert_eq!(s.name, "");
        assert_eq!(s.cmd, "claude --resume");
    }

    #[test]
    fn apply_char_ignores_control_chars() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Name;
        apply_char(&mut s, '\n');
        assert_eq!(s.name, "");
    }

    #[test]
    fn apply_backspace_pops_from_focused_text_field() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Cmd;
        apply_backspace(&mut s);
        assert_eq!(s.cmd, "claude --resum");
    }

    #[test]
    fn apply_backspace_noop_on_empty() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Cwd;
        apply_backspace(&mut s);
        assert_eq!(s.cwd, "");
    }

    #[test]
    fn apply_backspace_noop_on_radio() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Orchestrator;
        apply_backspace(&mut s);
        // cmd field untouched.
        assert_eq!(s.cmd, "claude --resume");
    }

    // --- cycle_value --------------------------------------------------------

    #[test]
    fn cycle_value_toggles_orchestrator() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Orchestrator;
        cycle_value(&mut s, Direction::Next);
        assert_eq!(s.orchestrator, Orchestrator::ClaudeCode);
        cycle_value(&mut s, Direction::Next);
        assert_eq!(s.orchestrator, Orchestrator::Cavekit);
    }

    #[test]
    fn cycle_value_cycles_layout_forward_and_back() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Layout;
        assert_eq!(s.layout, "builder");
        cycle_value(&mut s, Direction::Next);
        assert_eq!(s.layout, "cavekit");
        cycle_value(&mut s, Direction::Next);
        assert_eq!(s.layout, "builder");
        cycle_value(&mut s, Direction::Prev);
        assert_eq!(s.layout, "cavekit");
    }

    // --- build_spawn_argv ---------------------------------------------------

    #[test]
    fn build_spawn_argv_includes_all_flags() {
        let mut s = NewAgentState::default();
        s.cwd = "/home/user/proj".into();
        s.name = "proj".into();
        s.layout = "builder".into();
        s.cmd = "claude --resume".into();
        let argv = build_spawn_argv(&s);
        assert_eq!(
            argv,
            vec![
                "ark",
                "spawn",
                "--orchestrator",
                "cavekit",
                "--cwd",
                "/home/user/proj",
                "--name",
                "proj",
                "--layout",
                "builder",
                "--",
                "claude",
                "--resume",
            ]
        );
    }

    #[test]
    fn build_spawn_argv_uses_claude_code_slug() {
        let mut s = NewAgentState::default();
        s.orchestrator = Orchestrator::ClaudeCode;
        let argv = build_spawn_argv(&s);
        assert_eq!(argv[3], "claude-code");
    }

    #[test]
    fn build_spawn_argv_shlex_splits_quoted_cmd() {
        let mut s = NewAgentState::default();
        s.cmd = r#"bash -lc "cargo run -- --flag""#.into();
        let argv = build_spawn_argv(&s);
        // After `--`: bash, -lc, "cargo run -- --flag"
        let dash_idx = argv.iter().position(|x| x == "--").unwrap();
        assert_eq!(
            &argv[dash_idx + 1..],
            &["bash", "-lc", "cargo run -- --flag"]
        );
    }

    // --- build_new_agent_rows -----------------------------------------------

    #[test]
    fn build_new_agent_rows_has_title_and_footer() {
        let s = NewAgentState::default();
        let rows = build_new_agent_rows(&s);
        assert_eq!(rows[0], "New Agent");
        assert!(rows.last().unwrap().contains("Tab: next"));
        assert!(rows.last().unwrap().contains("Esc: cancel"));
    }

    #[test]
    fn build_new_agent_rows_renders_radio_selection() {
        let s = NewAgentState::default();
        let rows = build_new_agent_rows(&s);
        let orch_row = rows.iter().find(|r| r.contains("Orchestrator")).unwrap();
        assert!(orch_row.contains("[•] Cavekit"));
        assert!(orch_row.contains("[ ] Claude Code"));
    }

    // --- handle_new_agent_key -----------------------------------------------

    #[test]
    fn handle_key_tab_cycles_focus_forward() {
        let mut s = NewAgentState::default();
        let a = handle_new_agent_key(&mut s, KeyInput::Tab);
        assert_eq!(a, PickerAction::None);
        assert_eq!(s.focus, FormField::Cwd);
    }

    #[test]
    fn handle_key_shift_tab_cycles_focus_backward() {
        let mut s = NewAgentState::default();
        let a = handle_new_agent_key(&mut s, KeyInput::ShiftTab);
        assert_eq!(a, PickerAction::None);
        assert_eq!(s.focus, FormField::Submit);
    }

    #[test]
    fn handle_key_enter_submits() {
        let mut s = NewAgentState::default();
        let a = handle_new_agent_key(&mut s, KeyInput::Enter);
        assert_eq!(a, PickerAction::SubmitNewAgent);
    }

    #[test]
    fn handle_key_esc_cancels() {
        let mut s = NewAgentState::default();
        let a = handle_new_agent_key(&mut s, KeyInput::Esc);
        assert_eq!(a, PickerAction::CancelNewAgent);
    }

    #[test]
    fn handle_key_char_inserts_when_text_focused() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Name;
        handle_new_agent_key(&mut s, KeyInput::Char('h'));
        handle_new_agent_key(&mut s, KeyInput::Char('i'));
        assert_eq!(s.name, "hi");
    }

    #[test]
    fn handle_key_backspace_pops_when_text_focused() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Cmd;
        handle_new_agent_key(&mut s, KeyInput::Backspace);
        assert_eq!(s.cmd, "claude --resum");
    }

    #[test]
    fn handle_key_right_cycles_orchestrator() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Orchestrator;
        handle_new_agent_key(&mut s, KeyInput::Right);
        assert_eq!(s.orchestrator, Orchestrator::ClaudeCode);
    }

    #[test]
    fn handle_key_left_cycles_layout_backward() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Layout;
        handle_new_agent_key(&mut s, KeyInput::Left);
        // With ["builder", "cavekit"], Prev from "builder" → "cavekit".
        assert_eq!(s.layout, "cavekit");
    }

    #[test]
    fn handle_key_ctrl_f_on_cwd_sets_filepicker_flag() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Cwd;
        handle_new_agent_key(&mut s, KeyInput::CtrlF);
        assert!(s.open_filepicker);
    }

    #[test]
    fn handle_key_ctrl_f_on_non_cwd_ignored() {
        let mut s = NewAgentState::default();
        s.focus = FormField::Name;
        handle_new_agent_key(&mut s, KeyInput::CtrlF);
        assert!(!s.open_filepicker);
    }

    #[test]
    fn new_agent_state_with_cwd_defaults_name_to_basename() {
        let s = NewAgentState::with_cwd("/home/user/proj");
        assert_eq!(s.cwd, "/home/user/proj");
        assert_eq!(s.name, "proj");
        assert_eq!(s.layout, "builder");
        assert_eq!(s.cmd, "claude --resume");
        assert_eq!(s.focus, FormField::Orchestrator);
        assert_eq!(s.available_layouts, vec!["builder", "cavekit"]);
    }
}
