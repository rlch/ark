//! Picker state model (cavekit-plugin-picker R2).
//!
//! Pure, host-testable state types plus a handful of naive helpers. The
//! downstream tasks fill in behaviour:
//!
//! - T-101 populates [`PickerCache`] via the bootstrap state/socket scan.
//! - T-102 upgrades [`filter_matches`] to nucleo-matcher and adds fuzzy scoring.
//! - T-103 fetches the [`DetailState`] snapshot on demand.
//! - T-104 wires keyboard focus cycling for [`NewAgentState`].
//! - T-105 drives the [`ConfirmKill`] / [`Error`] transitions.
//!
//! Nothing in this module touches zellij-tile — it all compiles on the host
//! so the state machine can be exhaustively unit-tested without a wasm
//! target. Serde-derive is applied where the kit hints that future pipe
//! ingestion will deserialise into these shapes (R3 supervisor pipes).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level screen state machine.
///
/// Each variant owns its own substate so transitions are a single
/// `self.screen = ...` assignment — no scattered "which screen am I on"
/// flags. Variant names mirror the R2 acceptance-criteria enum; the `Help`
/// and `Error` arms stay on separate variants (vs. a single `Overlay`) to
/// keep the match-arm shape explicit for T-102's render dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerScreen {
    /// Main list / filter view (R4).
    List(ListState),
    /// Expanded detail for a single agent (R5). Only the id is stored;
    /// the full snapshot is fetched on-demand in T-103.
    Detail(DetailState),
    /// New-agent spawn form (R6). Populated by T-104 keystrokes.
    NewAgent(NewAgentState),
    /// Kill-confirmation modal (R7).
    ConfirmKill(ConfirmKillState),
    /// Rename prompt modal (R7 — `Ctrl+R` on a live agent). Captures the
    /// in-flight new-name buffer plus cursor position so typing keeps the
    /// same lossless round-trip behaviour the new-agent form gets.
    RenamePrompt(RenamePromptState),
    /// Resurrect prompt modal (R8 — Enter on a Crashed/Done agent).
    /// Asks the operator to confirm `y`/`n` before re-spawning via the
    /// T-106 pipeline.
    ResurrectPrompt(ResurrectPromptState),
    /// Help overlay (W5).
    Help,
    /// Error banner — one-off message, cleared on next key (R6 exec
    /// failure, R7 socket-connect failure, R3 permission denial).
    Error(ErrorState),
}

impl Default for PickerScreen {
    /// R2: the picker opens on the list screen.
    fn default() -> Self {
        PickerScreen::List(ListState::default())
    }
}

/// State for the main list screen.
///
/// Fields match R2 exactly: filter text, highlighted row index, and the
/// scroll offset used by [`apply_scroll`] to keep the selection visible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListState {
    /// Substring/fuzzy query typed into the filter line. Empty = match-all.
    pub filter: String,
    /// Index of the highlighted row within the *filtered* view. Callers
    /// must pass the filtered total to [`move_selection_up`] /
    /// [`move_selection_down`] or the clamp will be wrong.
    pub selected: usize,
    /// Number of rows scrolled past the top of the viewport. Adjusted by
    /// [`apply_scroll`] based on `visible_rows` from render.
    pub scroll_offset: usize,
    /// When true, printable keys append to [`Self::filter`] instead of
    /// being interpreted as bare-letter hotkeys (`r`, `j`, `k`, `h`, `l`,
    /// `N`, `?`). Toggled on by `/` and off by `Esc` — R9.
    pub filter_active: bool,
}

/// State for the expanded-detail screen.
///
/// Carries the agent id (required) plus the optional fresh snapshot fetched
/// from the supervisor's control socket on expand. `snapshot == None` with
/// `error == None` is the "fetch in-flight" state; once the fetch completes
/// exactly one of the two is populated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailState {
    /// Agent id whose detail to display. Must exist in
    /// `PickerCache::active` at render time.
    pub agent_id: String,
    /// Fresh snapshot pulled from the supervisor via `{"cmd":"Status"}`.
    /// `None` while the fetch is in-flight or before it ran.
    pub snapshot: Option<DetailSnapshot>,
    /// Transient connect/parse error for inline display. Cleared on the
    /// next successful fetch.
    pub error: Option<String>,
}

/// Full per-agent snapshot surfaced on the detail screen (R5).
///
/// Shape is deserialised from the supervisor's `Status` reply — the R1
/// serde_json ban means we parse with the hand-rolled extractors in
/// [`crate::bootstrap`]. Optional epoch-seconds fields stay `Option<u64>`
/// so missing values render as a dash rather than "0s ago".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailSnapshot {
    /// Zellij session name (from `spec.session`).
    pub session: String,
    /// Working directory (home-relative rendering happens at draw time).
    pub cwd: String,
    /// Orchestrator slug.
    pub orchestrator: String,
    /// Engine slug.
    pub engine: String,
    /// Phase string from the top-level `phase` field.
    pub phase: String,
    /// Current iteration (may not be surfaced by all orchestrators).
    pub iter: Option<u32>,
    /// Epoch-seconds of agent start.
    pub started_at: Option<u64>,
    /// Epoch-seconds of last event.
    pub last_event_at: Option<u64>,
    /// Epoch-seconds of most recent review round (reviewing orchestrator).
    pub last_review_at: Option<u64>,
    /// Short message from the most recent event.
    pub last_event: Option<String>,
}

/// Orchestrator choice for a spawn request (R6 first field).
///
/// Matches the `cavekit | claude-code` radio in W3. Kept as an enum so the
/// form validator can reject invalid values at compile time instead of at
/// `ark spawn` exec time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orchestrator {
    /// Cavekit-driven agents (`--orchestrator cavekit`).
    Cavekit,
    /// Raw Claude Code sessions (`--orchestrator claude-code`).
    ClaudeCode,
}

impl Default for Orchestrator {
    /// R6 W3 wireframe shows `[ cavekit ]` selected by default.
    fn default() -> Self {
        Orchestrator::Cavekit
    }
}

/// Which field of the new-agent form currently holds keyboard focus.
///
/// Ordered top-to-bottom per W3 so `Tab` maps to the next variant via the
/// enum's discriminant order. T-104 implements the actual cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    /// Orchestrator radio.
    Orchestrator,
    /// CWD text input. `Ctrl+f` overlay opens the filepicker plugin.
    Cwd,
    /// Agent name; default populated from `basename(cwd)` by T-104.
    Name,
    /// Zellij layout dropdown.
    Layout,
    /// Launch command, default `claude --resume`.
    Cmd,
    /// Submit button — Enter from here fires the spawn.
    Submit,
}

impl Default for FormField {
    /// Form opens with focus on the first field (R6 Tab-order origin).
    fn default() -> Self {
        FormField::Orchestrator
    }
}

/// State for the `Ctrl+n` new-agent form (R6 / W3).
///
/// Fields mirror the five inputs in the wireframe. T-104 fills in the
/// typing, cycling, and submission logic; this struct just holds the
/// values so the state transition into/out of the form is lossless
/// (users who tab away and back shouldn't lose partial input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentState {
    /// Orchestrator choice.
    pub orchestrator: Orchestrator,
    /// Working directory passed as `ark spawn --cwd`.
    pub cwd: String,
    /// Agent name passed as `ark spawn --name`.
    pub name: String,
    /// Zellij layout name passed as `ark spawn --layout`.
    pub layout: String,
    /// Launch command after the `--` separator.
    pub cmd: String,
    /// Currently focused field for keyboard input.
    pub focus: FormField,
    /// Layouts known to the plugin; drives the dropdown cycler. Populated
    /// on entering the NewAgent screen — defaults to
    /// `["builder", "cavekit"]` when scanning the layouts dir isn't
    /// available.
    pub available_layouts: Vec<String>,
    /// Stub flag raised when the user presses `Ctrl+F` on the Cwd field.
    /// The real filepicker integration is deferred; host-side tests assert
    /// this toggles.
    pub open_filepicker: bool,
}

impl Default for NewAgentState {
    fn default() -> Self {
        Self {
            orchestrator: Orchestrator::default(),
            cwd: String::new(),
            name: String::new(),
            layout: "builder".to_string(),
            cmd: "claude --resume".to_string(),
            focus: FormField::default(),
            available_layouts: vec!["builder".to_string(), "cavekit".to_string()],
            open_filepicker: false,
        }
    }
}

impl NewAgentState {
    /// Build a fresh `NewAgentState` seeded with `cwd`. `name` defaults to
    /// `basename(cwd)`. Used by the wasm layer when `Ctrl+N` fires from
    /// the list screen.
    pub fn with_cwd(cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        let name = crate::render_new_agent::basename_of(&cwd);
        Self {
            cwd,
            name,
            ..Self::default()
        }
    }
}

/// State for the Del confirmation modal (R7 W4).
///
/// The kill-scope (`kill` vs `kill + remove worktree`) is captured at the
/// key-press that dismisses the modal, not here — leaving it off keeps the
/// modal stateless and lets T-105 dispatch directly from the keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmKillState {
    /// Agent id the user is about to terminate.
    pub agent_id: String,
}

/// State for the `Ctrl+R` rename prompt (R7 W4).
///
/// `new_name` starts empty (not pre-filled with the current name — the R7
/// wireframe shows an input field the operator types into fresh) and
/// `focus_cursor` tracks the insertion point. T-105 only uses append /
/// backspace, so the cursor always sits at `new_name.chars().count()`; the
/// field is kept so a later polish task can add arrow-key cursor moves
/// without widening the struct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenamePromptState {
    /// Agent id whose name is being edited.
    pub agent_id: String,
    /// In-flight new name buffer.
    pub new_name: String,
    /// Character-index cursor position within `new_name`.
    pub focus_cursor: usize,
}

impl RenamePromptState {
    /// Construct a fresh prompt for `agent_id` with an empty name buffer.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            new_name: String::new(),
            focus_cursor: 0,
        }
    }
}

/// Reason variant carried alongside a resurrect prompt (R8).
///
/// `Crashed` fires for agents whose supervisor is no longer alive —
/// the prompt wording is "crashed — resurrect?". `TerminatedPhase`
/// fires for agents whose last published phase is `Done` / `Failed`
/// / `Killed` / `Timeout`, where the prompt becomes "is {phase} — spawn
/// a fresh replacement?". Keeping the discriminator on the state
/// instead of computing it at render time means the key handler only
/// looks at what's in hand, not at the cache it was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResurrectReason {
    /// Supervisor unreachable / PID dead. Agent lives in the
    /// resurrectable cache.
    Crashed,
    /// Agent's last phase was a terminal state (Done / Failed /
    /// Killed / Timeout). Re-spawn replaces it. The carried string
    /// is the raw phase value so the prompt can render it verbatim.
    TerminatedPhase(String),
}

/// State for the `Enter`-on-crashed / `Enter`-on-terminal resurrect prompt
/// (R8). Captures the agent id the prompt targets plus the reason variant
/// used to drive the prompt's wording. The action handler only needs to
/// know "confirm or cancel" — the lib.rs dispatcher looks up the cache
/// entry to produce the resurrect argv via the T-106 pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResurrectPromptState {
    /// Agent id whose spec will drive the resurrect.
    pub agent_id: String,
    /// Human-facing agent name (from the cache entry) used for the
    /// prompt's banner line.
    pub agent_name: String,
    /// Why we're prompting — drives the wording variant.
    pub reason: ResurrectReason,
}

impl ResurrectPromptState {
    /// Build a fresh prompt for `agent_id` with the given name + reason.
    pub fn new(
        agent_id: impl Into<String>,
        agent_name: impl Into<String>,
        reason: ResurrectReason,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            agent_name: agent_name.into(),
            reason,
        }
    }
}

/// State for the transient error banner (R6 exec failure, etc.).
///
/// Any key-press on the Error screen transitions back to List; that
/// behaviour lives in T-105's key handler, so this struct carries only
/// the message to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorState {
    /// Human-readable error message surfaced to the user.
    pub message: String,
}

/// Cached per-agent summary used by the list and detail screens.
///
/// Fields are the minimum the R4 wireframe needs to render a row plus a
/// few that the R5 detail header uses (cwd, started/last timestamps). The
/// progress tuple is `(done, total)` — `None` when the orchestrator does
/// not publish step counts (`—` in the wireframe).
///
/// Serde-derive is present so T-101's bootstrap scan and T-103's pipe
/// ingestion can deserialise status.json entries straight into this type
/// without an intermediate shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent id (matches the socket file's stem and state-dir name).
    pub id: String,
    /// Human-readable name from spec.json.
    pub name: String,
    /// Orchestrator label (`cavekit` / `claude-code`).
    pub orchestrator: String,
    /// Engine label (`claude-code`, etc.).
    pub engine: String,
    /// Phase string from status.json (`running`, `stalled`, `done`, ...).
    pub phase: String,
    /// Working directory; rendered home-relative by T-102.
    pub cwd: String,
    /// Current iteration, if the orchestrator publishes one.
    pub iter: Option<u32>,
    /// Epoch-seconds the agent started, if known.
    pub started_at: Option<u64>,
    /// Epoch-seconds of the most recent event, if any.
    pub last_event_at: Option<u64>,
    /// `(completed, total)` progress, if known.
    pub progress: Option<(u32, u32)>,
}

/// Separate active vs resurrectable caches.
///
/// R2: "Agents cache: `BTreeMap<AgentId, AgentSummary>` updated via pipe
/// messages + bootstrap read" and "Resurrectable agents: separate cache
/// for crashed agents (pid dead) found via state dir scan". BTreeMap gives
/// stable iteration order so row rendering doesn't jitter between redraws.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerCache {
    /// Agents with a live socket — switchable, killable, renamable.
    pub active: BTreeMap<String, AgentSummary>,
    /// Agents whose supervisor is dead but spec.json remains — eligible
    /// for R7 resurrect.
    pub resurrectable: BTreeMap<String, AgentSummary>,
}

// ---------------------------------------------------------------------------
// Pure helpers. All host-testable; no wasm imports.
// ---------------------------------------------------------------------------

/// Move the list selection up one row, saturating at 0.
///
/// `total` is the number of rows currently visible (post-filter). Passed
/// in so the helper can clamp a stale `selected` that overshoots the new
/// filtered size; callers that hand in `total == 0` get `selected = 0`.
pub fn move_selection_up(state: &mut ListState, total: usize) {
    if total == 0 {
        state.selected = 0;
        return;
    }
    // Clamp first in case `selected` is out of date (filter just shrunk
    // the list) — otherwise `saturating_sub` could leave us parked past
    // the new end.
    if state.selected >= total {
        state.selected = total - 1;
    }
    state.selected = state.selected.saturating_sub(1);
}

/// Move the list selection down one row, saturating at `total - 1`.
///
/// Mirror of [`move_selection_up`]; see that doc-comment for rationale on
/// the pre-clamp. `total == 0` leaves `selected = 0` (nothing to select).
pub fn move_selection_down(state: &mut ListState, total: usize) {
    if total == 0 {
        state.selected = 0;
        return;
    }
    let last = total - 1;
    if state.selected >= last {
        state.selected = last;
    } else {
        state.selected += 1;
    }
}

/// Adjust `scroll_offset` so the selected row is on-screen given
/// `visible_rows` rows of viewport.
///
/// Two cases:
/// - selected scrolled off the top → pull offset down to `selected`
/// - selected scrolled off the bottom → push offset up to
///   `selected - visible_rows + 1`
///
/// `visible_rows == 0` is a degenerate render size (header-only
/// terminal); we no-op rather than underflow.
pub fn apply_scroll(state: &mut ListState, visible_rows: usize) {
    if visible_rows == 0 {
        return;
    }
    if state.selected < state.scroll_offset {
        state.scroll_offset = state.selected;
    } else if state.selected >= state.scroll_offset + visible_rows {
        state.scroll_offset = state.selected + 1 - visible_rows;
    }
}

/// Case-insensitive substring match between `query` and the orchestrator /
/// name / id of `summary`.
///
/// Deliberately naive for T-100 — T-102 swaps in nucleo-matcher for real
/// fuzzy scoring. Empty query matches everything (so a freshly-opened
/// picker shows the full list).
pub fn filter_matches(summary: &AgentSummary, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_ascii_lowercase();
    // Match the composite "{orch}:{name}" the R4 wireframe shows in the
    // selector column, plus the raw id as a fallback so users can paste
    // ids from logs.
    let haystacks = [
        summary.name.to_ascii_lowercase(),
        summary.orchestrator.to_ascii_lowercase(),
        summary.id.to_ascii_lowercase(),
        format!(
            "{}:{}",
            summary.orchestrator.to_ascii_lowercase(),
            summary.name.to_ascii_lowercase()
        ),
    ];
    haystacks.iter().any(|h| h.contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- PickerScreen default ------------------------------------------------

    #[test]
    fn default_screen_is_list() {
        // R2: the picker opens on the list screen.
        match PickerScreen::default() {
            PickerScreen::List(state) => {
                assert_eq!(state, ListState::default());
            }
            other => panic!("expected PickerScreen::List, got {other:?}"),
        }
    }

    // --- move_selection_up ---------------------------------------------------

    #[test]
    fn move_up_saturates_at_zero() {
        let mut state = ListState {
            selected: 0,
            ..Default::default()
        };
        move_selection_up(&mut state, 5);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn move_up_decrements_by_one() {
        let mut state = ListState {
            selected: 3,
            ..Default::default()
        };
        move_selection_up(&mut state, 5);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn move_up_with_empty_total_parks_at_zero() {
        let mut state = ListState {
            selected: 7,
            ..Default::default()
        };
        move_selection_up(&mut state, 0);
        assert_eq!(state.selected, 0);
    }

    // --- move_selection_down -------------------------------------------------

    #[test]
    fn move_down_saturates_at_last() {
        let mut state = ListState {
            selected: 4,
            ..Default::default()
        };
        move_selection_down(&mut state, 5);
        assert_eq!(state.selected, 4);
    }

    #[test]
    fn move_down_increments_by_one() {
        let mut state = ListState {
            selected: 1,
            ..Default::default()
        };
        move_selection_down(&mut state, 5);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn move_down_clamps_stale_selection() {
        // Selection points past the end — simulates a filter that shrank
        // the list. The helper should land inside the new bounds.
        let mut state = ListState {
            selected: 42,
            ..Default::default()
        };
        move_selection_down(&mut state, 3);
        assert_eq!(state.selected, 2);
    }

    // --- apply_scroll --------------------------------------------------------

    #[test]
    fn apply_scroll_pulls_offset_down_when_selected_above() {
        let mut state = ListState {
            selected: 2,
            scroll_offset: 5,
            ..Default::default()
        };
        apply_scroll(&mut state, 10);
        assert_eq!(state.scroll_offset, 2);
    }

    #[test]
    fn apply_scroll_pushes_offset_up_when_selected_below() {
        let mut state = ListState {
            selected: 12,
            scroll_offset: 0,
            ..Default::default()
        };
        apply_scroll(&mut state, 5);
        // selected=12 with 5 visible rows → offset = 12 - 5 + 1 = 8
        assert_eq!(state.scroll_offset, 8);
    }

    #[test]
    fn apply_scroll_leaves_offset_alone_when_in_window() {
        let mut state = ListState {
            selected: 7,
            scroll_offset: 5,
            ..Default::default()
        };
        apply_scroll(&mut state, 5);
        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn apply_scroll_zero_visible_noop() {
        let mut state = ListState {
            selected: 3,
            scroll_offset: 2,
            ..Default::default()
        };
        apply_scroll(&mut state, 0);
        assert_eq!(state.scroll_offset, 2);
    }

    // --- filter_matches ------------------------------------------------------

    fn sample_summary() -> AgentSummary {
        AgentSummary {
            id: "abc123".to_string(),
            name: "MyFeat".to_string(),
            orchestrator: "Cavekit".to_string(),
            engine: "claude-code".to_string(),
            phase: "running".to_string(),
            cwd: "/tmp".to_string(),
            iter: None,
            started_at: None,
            last_event_at: None,
            progress: None,
        }
    }

    #[test]
    fn filter_empty_query_matches() {
        assert!(filter_matches(&sample_summary(), ""));
    }

    #[test]
    fn filter_substring_matches() {
        assert!(filter_matches(&sample_summary(), "feat"));
    }

    #[test]
    fn filter_missing_substring_does_not_match() {
        assert!(!filter_matches(&sample_summary(), "zzz"));
    }

    #[test]
    fn filter_is_case_insensitive() {
        // query cased opposite to haystack on both sides
        assert!(filter_matches(&sample_summary(), "MYFEAT"));
        assert!(filter_matches(&sample_summary(), "cavekit"));
    }

    #[test]
    fn filter_matches_composite_orch_name() {
        // R4's selector column shows `orchestrator:name`.
        assert!(filter_matches(&sample_summary(), "cavekit:myfeat"));
    }
}
