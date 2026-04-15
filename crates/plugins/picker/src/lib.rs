//! `ark-plugin-picker` — interactive zellij plugin that surfaces the agent
//! picker (list / detail / new / kill / help screens).
//!
//! This file lands the T-099 scaffold only; state, bootstrap, list render,
//! detail, forms, and distribution wiring arrive in T-100 through T-106. The
//! scaffold establishes the crate shape, dependency set, and the wasm-side
//! lifecycle glue so later tasks can fill in behaviour without touching build
//! configuration.
//!
//! T-100 layers the R2 state model on top of the scaffold: see the
//! [`state`] submodule for the [`PickerScreen`] enum, per-screen substate
//! structs, and [`PickerCache`] (active + resurrectable). All of those
//! types are pure Rust — the wasm plugin impl below stays untouched so
//! host-side tests exercise the state machine directly.
//!
//! Satisfies `context/kits/cavekit-plugin-picker.md` R1 acceptance criteria:
//!
//! - Crate `ark-plugin-picker` with `crate-type = ["cdylib"]` (see Cargo.toml).
//! - Build target `wasm32-wasip1` — driven by distribution wiring in T-106;
//!   host-side `cargo check` and workspace `cargo build` stay green because
//!   the `ZellijPlugin` impl (and `register_plugin!` macro expansion, which
//!   calls into `host_*` shims imported from the wasm `zellij` module) are
//!   gated behind `#[cfg(target_arch = "wasm32")]`.
//! - Dependencies: `zellij-tile`, `nucleo-matcher` (NOT `fuzzy-matcher` — R1
//!   mandates nucleo-matcher for its smaller wasm footprint), `serde`. The
//!   banned-by-R1 crates (`serde_json`, `humantime`, `chrono`) are deliberately
//!   absent; hand-rolled formatters land with later tasks.
//! - Permissions requested in `load()`:
//!   `ReadCliPipes`, `ChangeApplicationState`, `ReadApplicationState`,
//!   `MessageAndLaunchOtherPlugins`.
//! - Event subscriptions in `load()`:
//!   `Key`, `Timer`, `SessionUpdate`, `ModeUpdate`, `PermissionRequestResult`.
//!   (`PermissionRequestResult` is added so R2's state model can react to the
//!   grant/deny result — standard practice across ark plugins.)
//! - Plugin registers under the name `ark-picker` (see [`PLUGIN_NAME`]) and is
//!   wired through `zellij_tile::register_plugin!` inside the wasm module.
//!
//! # Target gating
//!
//! Same rationale as `ark-plugin-status`: zellij-tile's host shims link to
//! `host_run_plugin_command` which is only defined inside the wasm sandbox.
//! Gating the `ZellijPlugin` impl behind `cfg(target_arch = "wasm32")` lets
//! workspace-wide host builds compile this crate without resolving wasm-only
//! symbols. Host-side tests exercise the plain-Rust state via
//! [`Picker::new`].

pub mod bootstrap;
pub mod render_confirm;
pub mod render_detail;
pub mod render_list;
pub mod render_new_agent;
pub mod socket_cmd;
pub mod state;

pub use bootstrap::{
    Classification, REACHABILITY_TIMEOUT_MS, bootstrap as bootstrap_cache, check_reachable,
    classify, gc_stale_sockets, parse_agent_status_minimal, resolve_xdg_paths, scan_socket_dir,
    scan_state_dir,
};
pub use render_confirm::{
    handle_confirm_kill_key, handle_rename_prompt_key, render_confirm_kill, render_rename_prompt,
};
pub use render_detail::{
    DETAIL_CONT, DETAIL_INDENT, DETAIL_TREE, DetailError, build_detail_rows, format_humantime,
    handle_detail_key, home_rel, parse_status_response, query_agent_status,
};
pub use render_list::{
    KeyInput, PickerAction, build_footer, build_header, format_age, format_progress, format_row,
    fuzzy_filter_and_sort, fuzzy_haystack, handle_list_key, phase_extra, phase_icon,
};
pub use render_new_agent::{
    Direction, apply_backspace, apply_char, basename_of, build_new_agent_rows, build_spawn_argv,
    cycle_value, handle_new_agent_key, next_field, prev_field,
};
pub use socket_cmd::{
    SocketError, escape_json_string, forget_cmd, kill_cmd, rename_cmd, send_command,
};
pub use state::{
    AgentSummary, ConfirmKillState, DetailSnapshot, DetailState, ErrorState, FormField, ListState,
    NewAgentState, Orchestrator, PickerCache, PickerScreen, RenamePromptState, apply_scroll,
    filter_matches, move_selection_down, move_selection_up,
};

/// Registered plugin name used by supervisors when targeting `zellij pipe
/// --name`, matching R1's "load() registers pipe target: ark-picker" bullet.
///
/// Exposed as a constant so the dispatch side (supervisors / orchestrators)
/// and the ingestion filter (this plugin) share one source of truth.
pub const PLUGIN_NAME: &str = "ark-picker";

/// Root plugin state.
///
/// T-100 fills in the R2 state model: a [`PickerScreen`] discriminator, the
/// active + resurrectable [`PickerCache`], and the focused-session hint
/// supplied by `SessionUpdate` events. Later tasks only need to mutate
/// these fields — no further re-shaping of `Picker` should be required
/// before T-106.
#[derive(Debug, Default)]
pub struct Picker {
    /// Current UI screen + its substate. Defaults to
    /// `PickerScreen::List(ListState::default())` via
    /// [`PickerScreen::default`].
    pub screen: PickerScreen,
    /// Cached active and resurrectable agents, keyed by agent id. T-101
    /// populates this via state-dir + socket-dir scan; T-103 refreshes
    /// entries from supervisor pipe events.
    pub cache: PickerCache,
    /// The session zellij reports as focused. Used by the list screen to
    /// highlight / pin the current agent (R3/R4). `None` until the first
    /// `SessionUpdate` lands.
    pub focused_session: Option<String>,
    /// Cached list-state snapshot kept around while the detail screen is
    /// open so that the list render underneath the expand-tree retains
    /// the filter/selection/scroll the user left it in. Mirrors the
    /// session-manager expand-tree UX from R5 — Enter expands, ← collapses
    /// back to exactly the same list view.
    pub last_list_state: state::ListState,
}

impl Picker {
    /// Host-testable constructor. Kept non-`const` so future fields that need
    /// heap allocation (e.g. the agents `BTreeMap` from R2) can slot in
    /// without an API break.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the entire cache from the on-disk state + socket scans.
    ///
    /// Called on `load()` (for the initial population), on every 2 s
    /// [`Event::Timer`] tick, and whenever a socket-present-but-unreachable
    /// supervisor is detected so the stale GC pass has a chance to run.
    /// Caller decides whether to redraw based on the cache diff — we just
    /// replace the state wholesale here (T-101 keeps the merge naive; T-103
    /// adds pipe-driven fine-grained updates via [`Self::apply_pipe_update`]).
    ///
    /// Both paths are std-only so host tests exercise this directly.
    pub fn refresh_cache(&mut self, state_dir: &std::path::Path, runtime_dir: &std::path::Path) {
        self.cache = crate::bootstrap::bootstrap(state_dir, runtime_dir);
    }

    /// Merge a single supervisor-sourced [`AgentSummary`] into the active
    /// cache without touching the rest of the entries.
    ///
    /// R3's "Incremental updates: supervisors pipe to `ark-picker` target
    /// on every event; plugin updates its cache" — the pipe path owns the
    /// fine-grained fast path, while [`Self::refresh_cache`] is the 2 s
    /// safety net.
    ///
    /// If the summary's id previously lived in `resurrectable` (e.g. we
    /// classified it as crashed but the supervisor is actually alive and
    /// piping updates), we promote it to `active` — the pipe landing is
    /// itself the liveness signal per the kakoune `kak -l` aggregator
    /// model.
    pub fn apply_pipe_update(&mut self, summary: AgentSummary) {
        if summary.id.is_empty() {
            return;
        }
        self.cache.resurrectable.remove(&summary.id);
        self.cache.active.insert(summary.id.clone(), summary);
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::{
        ConfirmKillState, DetailState, ErrorState, KeyInput, NewAgentState, Picker, PickerAction,
        PickerScreen, RenamePromptState, bootstrap, build_detail_rows, build_footer, build_header,
        build_new_agent_rows, build_spawn_argv, forget_cmd, format_row, fuzzy_filter_and_sort,
        handle_confirm_kill_key, handle_detail_key, handle_list_key, handle_new_agent_key,
        handle_rename_prompt_key, kill_cmd, rename_cmd, render_confirm_kill, render_rename_prompt,
        socket_cmd::SocketError,
    };
    use zellij_tile::prelude::*;

    /// Selected-row highlight level — reused from the status-plugin
    /// convention so both plugins feel consistent in zellij's theme.
    const HIGHLIGHT_LEVEL: usize = 0;

    /// Translate a zellij `KeyWithModifier` into the picker's narrow
    /// [`KeyInput`] vocabulary. Unknown keys collapse to
    /// [`KeyInput::Other`].
    fn map_key(key: &KeyWithModifier) -> KeyInput {
        let ctrl = key.has_modifiers(&[KeyModifier::Ctrl]);
        let shift = key.has_modifiers(&[KeyModifier::Shift]);
        match key.bare_key {
            BareKey::Up => KeyInput::Up,
            BareKey::Down => KeyInput::Down,
            BareKey::Enter => KeyInput::Enter,
            BareKey::Esc => KeyInput::Esc,
            BareKey::Backspace => KeyInput::Backspace,
            BareKey::Delete => KeyInput::Delete,
            BareKey::Char('n') if ctrl => KeyInput::CtrlN,
            BareKey::Char('f') if ctrl => KeyInput::CtrlF,
            BareKey::Char('r') if ctrl => KeyInput::CtrlR,
            BareKey::Char('d') if ctrl => KeyInput::CtrlD,
            BareKey::Char('k') if key.has_no_modifiers() => KeyInput::Up,
            BareKey::Char('j') if key.has_no_modifiers() => KeyInput::Down,
            BareKey::Tab if shift => KeyInput::ShiftTab,
            BareKey::Tab => KeyInput::Tab,
            BareKey::Left => KeyInput::Left,
            BareKey::Right => KeyInput::Right,
            BareKey::Char(c) => KeyInput::Char(c),
            _ => KeyInput::Other,
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// R3: "2s timer: re-render for timing-sensitive fields … AND re-scan
    /// socket dir for liveness changes". Expressed as f64 because that's
    /// what `set_timeout` takes.
    const TIMER_INTERVAL_SECS: f64 = 2.0;

    /// Resolve the (state_dir, runtime_dir) pair via the process env.
    /// Pulled out as its own fn so `update`/`load` share it.
    fn wasm_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        bootstrap::resolve_xdg_paths(|k| std::env::var(k).ok())
    }

    /// Map a `SocketError` into the user-facing banner text R7 calls out.
    ///
    /// `Unreachable` maps to "agent no longer alive — press r to refresh
    /// [y/n]" per T-105 spec; Nak carries the supervisor-supplied error
    /// message inline; ProtocolError surfaces the raw bytes so operators
    /// can diagnose pipe-layer bugs without pawing through logs.
    fn socket_error_message(err: &SocketError) -> String {
        match err {
            SocketError::Unreachable => {
                "agent no longer alive — press r to refresh [y/n]".to_string()
            }
            SocketError::Nak(msg) => format!("supervisor rejected: {msg}"),
            SocketError::ProtocolError(raw) => format!("protocol error: {raw}"),
        }
    }

    /// Resolve the control socket path for `agent_id` inside the runtime
    /// dir discovered by [`wasm_paths`]. Centralised so kill / rename /
    /// forget and the detail-screen `query_agent_status` call site share
    /// one source of truth.
    fn agent_sock(agent_id: &str) -> std::path::PathBuf {
        let (_state_dir, runtime_dir) = wasm_paths();
        runtime_dir.join(format!("{agent_id}.sock"))
    }

    impl ZellijPlugin for Picker {
        fn load(&mut self, _configuration: std::collections::BTreeMap<String, String>) {
            // R1: request the four permissions the picker needs across its
            // lifecycle.
            // - ReadCliPipes: ingest supervisor updates piped to `ark-picker`
            //   (R3 incremental updates).
            // - ChangeApplicationState: switch sessions / open panes via
            //   zellij-tile actions when the user selects an agent (R4/R5).
            // - ReadApplicationState: enumerate sessions + panes for the
            //   list screen and detail screen bootstrap (R3/R4).
            // - MessageAndLaunchOtherPlugins: hand off to helper plugins
            //   (e.g. the new-agent form / confirm dialogs) per R6–R8.
            // Grant/deny for the batch surfaces via
            // `EventType::PermissionRequestResult`.
            request_permission(&[
                PermissionType::ReadCliPipes,
                PermissionType::ChangeApplicationState,
                PermissionType::ReadApplicationState,
                PermissionType::MessageAndLaunchOtherPlugins,
            ]);

            // R1: subscribe to the five event streams the picker consumes.
            // - Key: drive the interactive UI (filter typing, arrow keys,
            //   Enter to switch, Ctrl-N new, Ctrl-R rename, Del kill, ?
            //   help — see R4 footer).
            // - Timer: 2 s cadence for timing-sensitive fields and socket
            //   re-scan (R3).
            // - SessionUpdate: learn the focused session so the picker can
            //   highlight / pin it (R3/R4).
            // - ModeUpdate: react to zellij mode transitions (dismiss the
            //   picker when the user exits the plugin mode, etc.).
            // - PermissionRequestResult: react to grant/deny so later tasks
            //   can surface a warning screen instead of silently failing.
            subscribe(&[
                EventType::Key,
                EventType::Timer,
                EventType::SessionUpdate,
                EventType::ModeUpdate,
                EventType::PermissionRequestResult,
            ]);

            // R3: initial bootstrap — populate cache from state+socket
            // scans so the first render has something to show instead of
            // waiting up to 2 s for the first timer tick.
            let (state_dir, runtime_dir) = wasm_paths();
            if !state_dir.as_os_str().is_empty() {
                self.refresh_cache(&state_dir, &runtime_dir);
            }

            // Arm the 2 s cadence; subsequent ticks re-arm from `update`.
            set_timeout(TIMER_INTERVAL_SECS);
        }

        fn update(&mut self, event: Event) -> bool {
            match event {
                Event::Key(key) => {
                    let input = map_key(&key);
                    match self.screen {
                        PickerScreen::List(ref mut list_state) => {
                            let action = handle_list_key(list_state, &self.cache, input);
                            // Snapshot list state each key press — cheap,
                            // keeps `last_list_state` always fresh for any
                            // expand transition.
                            self.last_list_state = list_state.clone();
                            match action {
                                PickerAction::Close => {
                                    hide_self();
                                    false
                                }
                                PickerAction::OpenSession(name) => {
                                    switch_session(Some(&name));
                                    false
                                }
                                PickerAction::ExpandDetail(id) => {
                                    // T-103: transition to the detail
                                    // screen and kick off an on-demand
                                    // Status fetch over the agent's
                                    // socket. The fetch is host-IO; the
                                    // wasm build stubs `query_agent_
                                    // status` so we materialise an error
                                    // state here (better than silently
                                    // hanging).
                                    let mut detail = DetailState {
                                        agent_id: id.clone(),
                                        ..DetailState::default()
                                    };
                                    let (_state_dir, runtime_dir) = wasm_paths();
                                    let sock = runtime_dir.join(format!("{id}.sock"));
                                    match super::query_agent_status(&sock) {
                                        Ok(snap) => detail.snapshot = Some(snap),
                                        Err(e) => detail.error = Some(e.message().to_string()),
                                    }
                                    self.screen = PickerScreen::Detail(detail);
                                    true
                                }
                                PickerAction::NewAgent => {
                                    // T-104: open W3 with cwd seeded from
                                    // $PWD (best-effort) so `name` defaults
                                    // to `basename(cwd)`. The plugin runs
                                    // in the zellij cwd; this matches the
                                    // supervisor's expectation that spawn
                                    // happens "where the user is".
                                    let cwd = std::env::var("PWD").unwrap_or_default();
                                    self.screen =
                                        PickerScreen::NewAgent(NewAgentState::with_cwd(cwd));
                                    true
                                }
                                PickerAction::ConfirmKill(id) => {
                                    // T-105: Del on a live agent opens the
                                    // W4 confirm modal; the keystroke that
                                    // dismisses the modal dispatches the
                                    // actual socket command.
                                    self.screen = PickerScreen::ConfirmKill(ConfirmKillState {
                                        agent_id: id,
                                    });
                                    true
                                }
                                PickerAction::OpenRenamePrompt(id) => {
                                    // T-105: Ctrl+R opens the rename prompt
                                    // focused on the selected agent.
                                    self.screen =
                                        PickerScreen::RenamePrompt(RenamePromptState::new(id));
                                    true
                                }
                                PickerAction::ExecForget(id) => {
                                    // T-105: Ctrl+D fires Forget immediately
                                    // (no confirm). Unreachable → error
                                    // banner; everything else optimistically
                                    // refreshes the cache next tick.
                                    let sock = agent_sock(&id);
                                    match forget_cmd(&sock) {
                                        Ok(()) => {
                                            self.screen = PickerScreen::default();
                                        }
                                        Err(e) => {
                                            self.screen = PickerScreen::Error(ErrorState {
                                                message: socket_error_message(&e),
                                            });
                                        }
                                    }
                                    true
                                }
                                PickerAction::FilterChanged
                                | PickerAction::MoveUp
                                | PickerAction::MoveDown => true,
                                _ => true,
                            }
                        }
                        PickerScreen::NewAgent(ref mut form_state) => {
                            let action = handle_new_agent_key(form_state, input);
                            match action {
                                PickerAction::SubmitNewAgent => {
                                    // T-104: exec `ark spawn ...` as a
                                    // detached subprocess via
                                    // `run_command`. zellij-tile's
                                    // `run_command` is fire-and-forget —
                                    // no synchronous error channel, so we
                                    // optimistically transition back to
                                    // the list. Failures surface at the
                                    // next 2 s cache refresh (no new
                                    // agent = nothing in the cache).
                                    let argv = build_spawn_argv(form_state);
                                    let argv_refs: Vec<&str> =
                                        argv.iter().map(|s| s.as_str()).collect();
                                    run_command(&argv_refs, std::collections::BTreeMap::new());
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                PickerAction::CancelNewAgent => {
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                _ => true,
                            }
                        }
                        PickerScreen::Detail(ref mut detail_state) => {
                            let action = handle_detail_key(detail_state, input);
                            match action {
                                PickerAction::CollapseDetail => {
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                PickerAction::OpenSession(name) => {
                                    switch_session(Some(&name));
                                    false
                                }
                                PickerAction::ConfirmKill(id) => {
                                    // T-105: Del on Detail also opens the
                                    // confirm modal — parity with List.
                                    self.screen = PickerScreen::ConfirmKill(ConfirmKillState {
                                        agent_id: id,
                                    });
                                    true
                                }
                                _ => false,
                            }
                        }
                        PickerScreen::ConfirmKill(ref confirm_state) => {
                            let action = handle_confirm_kill_key(confirm_state, input);
                            match action {
                                PickerAction::ExecKill {
                                    agent_id,
                                    keep_worktree,
                                } => {
                                    let sock = agent_sock(&agent_id);
                                    // `keep_worktree=false` maps to the
                                    // uppercase-`Y` variant: ForceKill +
                                    // remove worktree. The helper takes
                                    // both bools so the semantics stay
                                    // symmetric with the R7 wireframe.
                                    let force = !keep_worktree;
                                    match kill_cmd(&sock, force, keep_worktree) {
                                        Ok(()) => {
                                            self.screen = PickerScreen::default();
                                        }
                                        Err(e) => {
                                            self.screen = PickerScreen::Error(ErrorState {
                                                message: socket_error_message(&e),
                                            });
                                        }
                                    }
                                    true
                                }
                                PickerAction::CancelKill => {
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                _ => false,
                            }
                        }
                        PickerScreen::RenamePrompt(ref mut rename_state) => {
                            let action = handle_rename_prompt_key(rename_state, input);
                            match action {
                                PickerAction::ExecRename(agent_id, new_name) => {
                                    let sock = agent_sock(&agent_id);
                                    match rename_cmd(&sock, &new_name) {
                                        Ok(()) => {
                                            self.screen = PickerScreen::default();
                                        }
                                        Err(e) => {
                                            self.screen = PickerScreen::Error(ErrorState {
                                                message: socket_error_message(&e),
                                            });
                                        }
                                    }
                                    true
                                }
                                PickerAction::CancelRename => {
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                // Typing / backspace: state already mutated
                                // in-place; just redraw.
                                PickerAction::None => true,
                                _ => true,
                            }
                        }
                        PickerScreen::Error(_) => {
                            // R7 "agent no longer alive — refresh? [y/n]".
                            // `y` fires a refresh and returns to List; `n`
                            // / Esc just returns to List. Any other key is
                            // a no-op so the banner stays visible.
                            match input {
                                KeyInput::Char('y') | KeyInput::Char('r') => {
                                    let (state_dir, runtime_dir) = wasm_paths();
                                    if !state_dir.as_os_str().is_empty() {
                                        self.refresh_cache(&state_dir, &runtime_dir);
                                    }
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                KeyInput::Char('n') | KeyInput::Esc => {
                                    self.screen = PickerScreen::default();
                                    true
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    }
                }
                Event::Timer(_elapsed) => {
                    // R3: 2 s timer re-scans state + sockets. We always
                    // re-arm so the cadence stays steady regardless of
                    // whether the scan found anything worth redrawing.
                    let (state_dir, runtime_dir) = wasm_paths();
                    let changed = if !state_dir.as_os_str().is_empty() {
                        let prev = self.cache.clone();
                        self.refresh_cache(&state_dir, &runtime_dir);
                        prev != self.cache
                    } else {
                        false
                    };
                    set_timeout(TIMER_INTERVAL_SECS);
                    changed
                }
                Event::SessionUpdate(_sessions, _resurrectable) => false,
                Event::ModeUpdate(_mode_info) => false,
                Event::PermissionRequestResult(_status) => false,
                _ => false,
            }
        }

        fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
            // R3: incremental updates — supervisors pipe a single-agent
            // JSON payload to `--name ark-picker` on every event. We reuse
            // the bootstrap parser so the hand-rolled JSON reader has one
            // call-site. Unknown pipe names / unparseable payloads are
            // silently ignored.
            if pipe_message.name != super::PLUGIN_NAME {
                return false;
            }
            let Some(payload) = pipe_message.payload else {
                return false;
            };
            let Some(summary) = bootstrap::parse_agent_status_minimal(&payload) else {
                return false;
            };
            self.apply_pipe_update(summary);
            true
        }

        fn render(&mut self, rows: usize, cols: usize) {
            // T-102: render the list screen. T-103 overlays the detail
            // expand-tree under the selected row when the picker is on the
            // Detail screen.
            match &self.screen {
                PickerScreen::List(list_state) => {
                    render_list_screen(
                        &self.cache,
                        list_state,
                        &self.focused_session,
                        None::<&super::DetailState>,
                        rows,
                        cols,
                    );
                }
                PickerScreen::Detail(detail_state) => {
                    render_list_screen(
                        &self.cache,
                        &self.last_list_state,
                        &self.focused_session,
                        Some(detail_state),
                        rows,
                        cols,
                    );
                }
                PickerScreen::NewAgent(form_state) => {
                    render_new_agent_screen(form_state, rows, cols);
                }
                PickerScreen::ConfirmKill(confirm_state) => {
                    render_confirm_kill_screen(&confirm_state.agent_id, rows, cols);
                }
                PickerScreen::RenamePrompt(rename_state) => {
                    render_rename_prompt_screen(rename_state, rows, cols);
                }
                PickerScreen::Error(err) => {
                    let text = Text::new(&format!("Error: {}", err.message));
                    print_text_with_coordinates(text, 0, 0, Some(cols), Some(1));
                }
                _ => {
                    let text = Text::new("(screen not yet implemented)");
                    print_text_with_coordinates(text, 0, 0, Some(cols), Some(1));
                }
            }
        }
    }

    fn render_new_agent_screen(state: &NewAgentState, rows: usize, cols: usize) {
        if rows == 0 || cols == 0 {
            return;
        }
        let body = build_new_agent_rows(state);
        for (y, line) in body.iter().enumerate() {
            if y >= rows {
                break;
            }
            print_text_with_coordinates(Text::new(line), 0, y, Some(cols), Some(1));
        }
    }

    /// T-105 W4: render the kill-confirm modal. The [`render_confirm_kill`]
    /// helper returns the already-laid-out lines so this wrapper just
    /// feeds them to `print_text_with_coordinates`.
    fn render_confirm_kill_screen(agent_id: &str, rows: usize, cols: usize) {
        if rows == 0 || cols == 0 {
            return;
        }
        let body = render_confirm_kill(agent_id);
        for (y, line) in body.iter().enumerate() {
            if y >= rows {
                break;
            }
            print_text_with_coordinates(Text::new(line), 0, y, Some(cols), Some(1));
        }
    }

    /// T-105 W4: render the rename prompt. Mirrors
    /// [`render_confirm_kill_screen`].
    fn render_rename_prompt_screen(state: &RenamePromptState, rows: usize, cols: usize) {
        if rows == 0 || cols == 0 {
            return;
        }
        let body = render_rename_prompt(state);
        for (y, line) in body.iter().enumerate() {
            if y >= rows {
                break;
            }
            print_text_with_coordinates(Text::new(line), 0, y, Some(cols), Some(1));
        }
    }

    // Silence dead_code warning when ErrorState isn't otherwise referenced in
    // wasm_plugin — it is used by future T-105 wiring.
    #[allow(dead_code)]
    fn _keep_error_state_used(_e: &ErrorState) {}

    fn render_list_screen(
        cache: &super::PickerCache,
        list_state: &super::ListState,
        focused: &Option<String>,
        detail: Option<&super::DetailState>,
        rows: usize,
        cols: usize,
    ) {
        if rows == 0 || cols == 0 {
            return;
        }
        // Header (row 0).
        let header = build_header(cache.active.len(), cache.resurrectable.len());
        print_text_with_coordinates(Text::new(&header), 0, 0, Some(cols), Some(1));

        // Filter line (row 1). Show cursor via trailing underscore.
        let filter_line = format!("filter: {}_", list_state.filter);
        print_text_with_coordinates(Text::new(&filter_line), 0, 1, Some(cols), Some(1));

        // Footer (last row).
        let footer_row = rows.saturating_sub(1);
        let footer = build_footer();
        print_text_with_coordinates(Text::new(&footer), 0, footer_row, Some(cols), Some(1));

        // Visible rows: between row 2 and footer_row (exclusive).
        let first_row_y = 2usize;
        if footer_row <= first_row_y {
            return;
        }
        let visible = footer_row - first_row_y;
        let now = now_ms();
        let filtered = fuzzy_filter_and_sort(cache, &list_state.filter);
        // `y` tracks the next available render row; bumped by both agent
        // rows and detail-tree rows so the expand-tree pushes later rows
        // down — matches the zellij session-manager UX called out in R5.
        let mut y = first_row_y;
        for (i, (id, _score)) in filtered
            .iter()
            .skip(list_state.scroll_offset)
            .take(visible)
            .enumerate()
        {
            if y >= footer_row {
                break;
            }
            let is_resurrectable = cache.resurrectable.contains_key(id.as_str());
            let summary_opt = if is_resurrectable {
                cache.resurrectable.get(id.as_str())
            } else {
                cache.active.get(id.as_str())
            };
            let Some(summary) = summary_opt else { continue };
            let idx = list_state.scroll_offset + i;
            let is_selected = idx == list_state.selected;
            let is_focused = focused.as_deref() == Some(summary.name.as_str());
            let row_text = format_row(
                summary,
                is_selected,
                is_focused,
                is_resurrectable,
                now,
                cols,
            );
            let mut text = Text::new(&row_text);
            if is_selected {
                text = text.color_range(HIGHLIGHT_LEVEL, ..);
            }
            print_text_with_coordinates(text, 0, y, Some(cols), Some(1));
            y += 1;

            // Nested detail tree: draw directly beneath the agent row
            // whose id matches the open detail screen.
            if let Some(det) = detail {
                if det.agent_id == *id {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let rows = build_detail_rows(det, &home, now);
                    for body in rows {
                        if y >= footer_row {
                            break;
                        }
                        print_text_with_coordinates(Text::new(&body), 0, y, Some(cols), Some(1));
                        y += 1;
                    }
                }
            }
        }
    }

    register_plugin!(Picker);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: the host-side constructor returns a value and does not
    /// panic. More meaningful tests land with the T-100 state model.
    #[test]
    fn picker_new_constructs() {
        let _picker = Picker::new();
    }

    #[test]
    fn apply_pipe_update_inserts_into_active() {
        let mut p = Picker::new();
        let summary = AgentSummary {
            id: "abc".into(),
            name: "auth".into(),
            phase: "running".into(),
            ..AgentSummary::default()
        };
        p.apply_pipe_update(summary);
        assert_eq!(p.cache.active.len(), 1);
        assert!(p.cache.active.contains_key("abc"));
    }

    #[test]
    fn apply_pipe_update_promotes_resurrectable_to_active() {
        let mut p = Picker::new();
        p.cache.resurrectable.insert(
            "z".into(),
            AgentSummary {
                id: "z".into(),
                phase: "crashed".into(),
                ..AgentSummary::default()
            },
        );
        p.apply_pipe_update(AgentSummary {
            id: "z".into(),
            phase: "running".into(),
            ..AgentSummary::default()
        });
        assert!(p.cache.active.contains_key("z"));
        assert!(!p.cache.resurrectable.contains_key("z"));
    }

    #[test]
    fn apply_pipe_update_ignores_empty_id() {
        let mut p = Picker::new();
        p.apply_pipe_update(AgentSummary::default());
        assert!(p.cache.active.is_empty());
    }
}
