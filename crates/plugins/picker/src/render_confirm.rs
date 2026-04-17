//! Picker confirm-kill + rename-prompt rendering + key handling
//! (cavekit-plugin-picker R7 / W4).
//!
//! Pure host Rust — the wasm `render` method in [`crate`] calls these
//! helpers and hands the resulting strings to `Text::new(...)` /
//! `print_text_with_coordinates`. Mirrors the split already established by
//! [`crate::render_list`] — one module per screen, all logic host-testable.
//!
//! # Acceptance criteria mapping (`cavekit-plugin-picker.md` R7)
//!
//! - Del → ConfirmKill screen `[y=Kill][Y=Kill+worktree][n=cancel]`:
//!   [`render_confirm_kill`] + [`handle_confirm_kill_key`].
//! - Ctrl+R prompt that opens a rename input:
//!   [`render_rename_prompt`] + [`handle_rename_prompt_key`].
//! - Errors surface via [`PickerAction::CancelKill`] / `CancelRename` back
//!   to lib.rs, which decides whether to display an `ErrorState` banner.
//!
//! # Key-handler contract
//!
//! Both handlers are pure with respect to the screen they own — they
//! return a [`PickerAction`] the wasm layer dispatches to socket helpers
//! in [`crate::socket_cmd`]. They only mutate state they own (rename
//! buffer / cursor); they never touch the cache or the list state.

use crate::render_list::{KeyInput, PickerAction};
use crate::state::{ConfirmKillState, RenamePromptState};

/// Render the kill-confirmation modal as a short box. The wasm layer
/// writes each line with `print_text_with_coordinates` starting at row 0.
///
/// The box deliberately stays narrow (fits into the picker's column
/// budget) so a mid-sized terminal shows it without wrapping. Legends
/// follow the W4 wireframe exactly so operators who memorised the kit
/// bindings don't have to re-learn them at the modal.
pub fn render_confirm_kill(agent_id: &str) -> Vec<String> {
    vec![
        "┌─ Confirm kill ─┐".to_string(),
        format!("Agent: {agent_id}"),
        "[y] Kill (keep worktree)   [Y] Kill + worktree   [n] Cancel".to_string(),
        "└────────────────┘".to_string(),
    ]
}

/// Render the rename prompt as a short input-field UI. Cursor is shown as
/// a trailing underscore (matches the list-screen filter row from T-102).
///
/// The prompt includes the agent id so the operator always knows which
/// agent they're renaming — easy mistake to make when multiple pickers
/// are in play.
pub fn render_rename_prompt(state: &RenamePromptState) -> Vec<String> {
    vec![
        "┌─ Rename agent ─┐".to_string(),
        format!("Agent: {}", state.agent_id),
        format!("New name: {}_", state.new_name),
        "[Enter] confirm   [Esc] cancel".to_string(),
        "└────────────────┘".to_string(),
    ]
}

/// Pure key handler for the confirm-kill modal.
///
/// Bindings (R7 W4):
/// * lowercase `y` → `ExecKill { keep_worktree: true }` (Kill, preserve worktree)
/// * uppercase `Y` → `ExecKill { keep_worktree: false }` (Kill + remove worktree)
/// * `n` or `Esc` → `CancelKill`
/// * anything else → `None`
///
/// F-607: both `y` and `Y` dispatch the graceful `Kill` command through
/// the socket — only the worktree disposition differs. Earlier code
/// escalated `Y` to `ForceKill`; the modal legend says "Kill + worktree",
/// not "force kill".
///
/// The handler is immutable — the modal has no in-flight state beyond the
/// agent id, which never changes across key presses, so `&ConfirmKillState`
/// is enough.
pub fn handle_confirm_kill_key(state: &ConfirmKillState, key: KeyInput) -> PickerAction {
    match key {
        KeyInput::Char('y') => PickerAction::ExecKill {
            agent_id: state.agent_id.clone(),
            keep_worktree: true,
        },
        KeyInput::Char('Y') => PickerAction::ExecKill {
            agent_id: state.agent_id.clone(),
            keep_worktree: false,
        },
        KeyInput::Char('n') | KeyInput::Esc => PickerAction::CancelKill,
        _ => PickerAction::None,
    }
}

/// Pure key handler for the rename prompt.
///
/// Bindings (R7 W4):
/// * `Char(c)` → append to `new_name`, advance cursor.
/// * `Backspace` → pop last char from `new_name`, retreat cursor (saturating).
/// * `Enter` → `ExecRename(agent_id, new_name)` — only if `new_name` is
///   non-empty; otherwise `None` so Enter on an empty buffer is a no-op.
/// * `Esc` → `CancelRename`.
///
/// Returned `new_name` is cloned so the action payload outlives the
/// state (the wasm layer drops `RenamePromptState` on the successful
/// transition back to List).
pub fn handle_rename_prompt_key(state: &mut RenamePromptState, key: KeyInput) -> PickerAction {
    match key {
        KeyInput::Char(c) if !c.is_control() => {
            state.new_name.push(c);
            state.focus_cursor = state.new_name.chars().count();
            PickerAction::None
        }
        KeyInput::Backspace => {
            if state.new_name.pop().is_some() {
                state.focus_cursor = state.new_name.chars().count();
            }
            PickerAction::None
        }
        KeyInput::Enter => {
            if state.new_name.is_empty() {
                PickerAction::None
            } else {
                PickerAction::ExecRename(state.agent_id.clone(), state.new_name.clone())
            }
        }
        KeyInput::Esc => PickerAction::CancelRename,
        _ => PickerAction::None,
    }
}

// ---------------------------------------------------------------------------
// Tests — host-side only.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- render_confirm_kill -----------------------------------------------

    #[test]
    fn render_confirm_kill_includes_agent_id() {
        let rows = render_confirm_kill("abc123");
        let blob = rows.join("\n");
        assert!(blob.contains("abc123"), "rows = {rows:?}");
        assert!(blob.contains("Confirm kill"));
        assert!(blob.contains("[y]"));
        assert!(blob.contains("[Y]"));
        assert!(blob.contains("[n]"));
    }

    // --- render_rename_prompt ----------------------------------------------

    #[test]
    fn render_rename_prompt_shows_current_buffer() {
        let state = RenamePromptState {
            agent_id: "abc".into(),
            new_name: "newname".into(),
            focus_cursor: 7,
        };
        let rows = render_rename_prompt(&state);
        let blob = rows.join("\n");
        assert!(blob.contains("Rename agent"));
        assert!(blob.contains("abc"));
        assert!(blob.contains("newname"));
    }

    // --- handle_confirm_kill_key -------------------------------------------

    fn ckill(id: &str) -> ConfirmKillState {
        ConfirmKillState {
            agent_id: id.into(),
        }
    }

    #[test]
    fn confirm_kill_y_lowercase_keeps_worktree() {
        let st = ckill("a");
        assert_eq!(
            handle_confirm_kill_key(&st, KeyInput::Char('y')),
            PickerAction::ExecKill {
                agent_id: "a".into(),
                keep_worktree: true,
            }
        );
    }

    #[test]
    fn confirm_kill_shift_y_removes_worktree() {
        let st = ckill("a");
        assert_eq!(
            handle_confirm_kill_key(&st, KeyInput::Char('Y')),
            PickerAction::ExecKill {
                agent_id: "a".into(),
                keep_worktree: false,
            }
        );
    }

    #[test]
    fn confirm_kill_n_cancels() {
        let st = ckill("a");
        assert_eq!(
            handle_confirm_kill_key(&st, KeyInput::Char('n')),
            PickerAction::CancelKill
        );
    }

    #[test]
    fn confirm_kill_esc_cancels() {
        let st = ckill("a");
        assert_eq!(
            handle_confirm_kill_key(&st, KeyInput::Esc),
            PickerAction::CancelKill
        );
    }

    #[test]
    fn confirm_kill_other_is_noop() {
        let st = ckill("a");
        assert_eq!(
            handle_confirm_kill_key(&st, KeyInput::Char('q')),
            PickerAction::None
        );
    }

    // --- handle_rename_prompt_key ------------------------------------------

    fn rprompt(id: &str) -> RenamePromptState {
        RenamePromptState::new(id)
    }

    #[test]
    fn rename_char_appends_to_buffer() {
        let mut st = rprompt("a");
        handle_rename_prompt_key(&mut st, KeyInput::Char('x'));
        handle_rename_prompt_key(&mut st, KeyInput::Char('y'));
        assert_eq!(st.new_name, "xy");
        assert_eq!(st.focus_cursor, 2);
    }

    #[test]
    fn rename_backspace_pops() {
        let mut st = RenamePromptState {
            agent_id: "a".into(),
            new_name: "abc".into(),
            focus_cursor: 3,
        };
        handle_rename_prompt_key(&mut st, KeyInput::Backspace);
        assert_eq!(st.new_name, "ab");
        assert_eq!(st.focus_cursor, 2);
    }

    #[test]
    fn rename_backspace_on_empty_is_noop() {
        let mut st = rprompt("a");
        let action = handle_rename_prompt_key(&mut st, KeyInput::Backspace);
        assert_eq!(action, PickerAction::None);
        assert_eq!(st.new_name, "");
    }

    #[test]
    fn rename_enter_with_buffer_execs() {
        let mut st = RenamePromptState {
            agent_id: "a".into(),
            new_name: "newname".into(),
            focus_cursor: 7,
        };
        assert_eq!(
            handle_rename_prompt_key(&mut st, KeyInput::Enter),
            PickerAction::ExecRename("a".into(), "newname".into())
        );
    }

    #[test]
    fn rename_enter_empty_is_noop() {
        let mut st = rprompt("a");
        assert_eq!(
            handle_rename_prompt_key(&mut st, KeyInput::Enter),
            PickerAction::None
        );
    }

    #[test]
    fn rename_esc_cancels() {
        let mut st = rprompt("a");
        assert_eq!(
            handle_rename_prompt_key(&mut st, KeyInput::Esc),
            PickerAction::CancelRename
        );
    }

    #[test]
    fn rename_control_char_ignored() {
        let mut st = rprompt("a");
        // `\x01` is a control char; our handler should not append it.
        handle_rename_prompt_key(&mut st, KeyInput::Char('\x01'));
        assert_eq!(st.new_name, "");
    }
}
