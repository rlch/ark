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
pub mod state;

pub use bootstrap::{
    Classification, REACHABILITY_TIMEOUT_MS, bootstrap as bootstrap_cache, check_reachable,
    classify, gc_stale_sockets, parse_agent_status_minimal, resolve_xdg_paths, scan_socket_dir,
    scan_state_dir,
};
pub use state::{
    AgentSummary, ConfirmKillState, DetailState, ErrorState, FormField, ListState, NewAgentState,
    Orchestrator, PickerCache, PickerScreen, apply_scroll, filter_matches, move_selection_down,
    move_selection_up,
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
    use super::{Picker, bootstrap};
    use zellij_tile::prelude::*;

    /// R3: "2s timer: re-render for timing-sensitive fields … AND re-scan
    /// socket dir for liveness changes". Expressed as f64 because that's
    /// what `set_timeout` takes.
    const TIMER_INTERVAL_SECS: f64 = 2.0;

    /// Resolve the (state_dir, runtime_dir) pair via the process env.
    /// Pulled out as its own fn so `update`/`load` share it.
    fn wasm_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        bootstrap::resolve_xdg_paths(|k| std::env::var(k).ok())
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
                Event::Key(_key) => false,
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

        fn render(&mut self, _rows: usize, _cols: usize) {
            // Scaffold stub: no-op render. T-102 lands the list screen per
            // R4. Leaving render empty here is intentional — a placeholder
            // string would flash in the first iteration's compiled artefact
            // and then have to be unwound.
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
