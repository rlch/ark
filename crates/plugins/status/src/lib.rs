//! `ark-plugin-status` — zellij status-bar plugin for ark.
//!
//! Satisfies `context/kits/cavekit-plugin-status.md` R1–R2 acceptance criteria:
//!
//! - Crate name `ark-plugin-status` with `crate-type = ["cdylib"]` (see Cargo.toml).
//! - Build target `wasm32-wasip1` is driven by distribution wiring (T-098 / T-130);
//!   host-side `cargo check` and workspace `cargo build` stay green.
//! - Dependencies: `zellij-tile`, `serde`, `serde_json` (see Cargo.toml).
//! - `load()` calls `request_permission(&[PermissionType::ReadCliPipes])` and
//!   `subscribe(&[EventType::Timer, EventType::PermissionRequestResult])`,
//!   then arms the first 1 Hz timer via `set_timeout(1.0)`.
//! - Plugin registers under the name `ark-status` (see [`PLUGIN_NAME`]) and is
//!   wired through [`zellij_tile::register_plugin!`].
//! - R2 pipe ingestion: filters on `message.name == "ark-status"`, parses JSON
//!   payload into [`StatusSummary`], upserts keyed on `agent_id` into a
//!   deterministic `BTreeMap`, and evicts entries stale for ≥60 minutes on
//!   each 1 Hz timer tick.
//!
//! R3 (render), R4 (filesystem fallback), R5 (distribution) are still stubbed
//! here — those land in T-096/T-097/T-098.
//!
//! # Target gating
//!
//! `zellij-tile`'s host shims call `extern "C" fn host_run_plugin_command`
//! imported from the wasm `zellij` module (`#[link(wasm_import_module = ...)]`).
//! On non-wasm targets that symbol is undefined and linking the cdylib fails.
//! The [`ZellijPlugin`] impl (and the `register_plugin!` expansion, which
//! calls into `host_*` shims) are therefore gated behind
//! `#[cfg(target_arch = "wasm32")]`. Host builds still compile this crate so
//! workspace-wide `cargo build` stays green; they just don't link the
//! wasm-only symbols. The cache + eviction logic lives in plain helpers so
//! host-side tests can exercise it without a wasm runtime.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod chip;
pub mod fs_scan;

pub use chip::{
    CHIP_SEPARATOR_WIDTH, Chip, Phase, Severity, build_chip, char_display_width, fit_chips,
    phase_from_str, phase_icon, phase_severity,
};
pub use fs_scan::{merge_fs_scan, resolve_state_dir, scan_state_dir};

/// Registered plugin name used by supervisors when targeting `zellij pipe --name`.
///
/// Supervisors publish agent status updates to this pipe target; see
/// cavekit-plugin-status R2. Defined as a constant so the dispatch side and the
/// ingestion filter share a single source of truth.
pub const PLUGIN_NAME: &str = "ark-status";

/// Extension name used by the scene `use "status"` resolver (T-10.10).
///
/// Distinct from [`PLUGIN_NAME`] (`ark-status`) — the zellij-side
/// pipe target — because the scene-layer name drops the vendor
/// prefix so user scenes can write `use "status"` idiomatically.
pub const EXTENSION_NAME: &str = "status";

/// Extension's range of supported ark versions. Matches the workspace
/// pre-v1.0 line.
pub const ARK_RANGE: &str = ">=0.1, <1.0";

/// Sidecar scene fragment shipped alongside the status plugin.
///
/// Embedded via `include_str!` so the fragment ships in the host
/// binary's data section. Same rationale as picker — off-box
/// installations drop the physical `scene.kdl` next to the wasm.
pub const SIDECAR_SCENE_KDL: &str = include_str!("../scene.kdl");

/// Build the [`ExtensionMetadata`] the scene compiler registers when
/// it resolves `use "status"`.
///
/// Status is event-only — it consumes supervisor pipe messages and
/// renders; no user-dispatched intents. Events advertised here let
/// the scene compiler validate `on "UserEvent:status.*"` selectors
/// against the known-event set.
pub fn status_metadata() -> ark_ext_metadata::ExtensionMetadata {
    use ark_ext_metadata::{CapabilitySet, ConfigSchema, EventDecl, StringNode};
    ark_ext_metadata::ExtensionMetadata {
        name: StringNode::new(EXTENSION_NAME),
        version: StringNode::new(env!("CARGO_PKG_VERSION")),
        ark_range: StringNode::new(ARK_RANGE),
        zellij_range: StringNode::new(">=0.44, <0.45"),
        requires: vec![],
        intents: vec![],
        events: vec![EventDecl {
            name: "status.updated".into(),
            payload_schema: StringNode::new("{}"),
        }],
        views: vec![],
        config: ConfigSchema::default(),
        capabilities: CapabilitySet::from_strs(&["ui.status-bar"]),
        config_sections: vec![],
        reload_gates: vec![],
    }
}

ark_ext_metadata::register_extension!(status_metadata);

#[cfg(test)]
mod ext_metadata_tests {
    use super::*;

    /// T-10.10: status ships no intents (pipe-driven only) and one
    /// event (`status.updated`).
    #[test]
    fn status_metadata_surface() {
        let m = status_metadata();
        assert_eq!(m.name.value, "status");
        assert!(m.intents.is_empty());
        let events: Vec<&str> = m.events.iter().map(|e| e.name.as_str()).collect();
        assert!(events.contains(&"status.updated"));
    }

    /// T-10.10: the sidecar `scene.kdl` ships the status-bar mount.
    #[test]
    fn sidecar_scene_contains_plugin_block() {
        assert!(SIDECAR_SCENE_KDL.contains("plugin \"status\""));
        assert!(SIDECAR_SCENE_KDL.contains("mount \"status-bar\""));
    }

    /// T-10.10: the `register_extension!` macro expanded into a
    /// reachable `ark_ext_metadata()` entry point.
    #[test]
    fn register_extension_macro_produced_entry_point() {
        let m = ark_ext_metadata();
        assert_eq!(m.name.value, "status");
    }
}

/// Eviction TTL for done/crashed agents per cavekit-plugin-status R2
/// ("Keep last 60 minutes … then evict"). Expressed in milliseconds so it
/// composes with the epoch-ms timestamps on [`StatusSummary::updated_at`].
pub const EVICTION_TTL_MS: u64 = 60 * 60 * 1000;

/// Per-agent status summary held by the plugin.
///
/// Mirrors the JSON envelope documented in cavekit-plugin-status R2. Fields
/// are kept flat and stringly-typed on purpose: the wasm target must not drag
/// in host-only dependencies (e.g. `ark-types::AgentId`), and `phase` carries
/// the serialized form of the state enum so render logic in T-096 can drive
/// icon selection without re-parsing.
///
/// `#[serde(default)]` keeps the plugin tolerant to future-older supervisors
/// that omit optional fields — an unknown `last_event` is not a reason to
/// drop the whole status update.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSummary {
    /// Stable agent identifier (the ULID-shaped string from `ark-types`).
    #[serde(default)]
    pub agent_id: String,
    /// Human-facing label (e.g. `auth`, `payments`).
    #[serde(default)]
    pub name: String,
    /// Orchestrator slug (e.g. `cavekit`, `claude-code`).
    #[serde(default)]
    pub orchestrator: String,
    /// Serialized phase (`running|idle|prompting|reviewing|done|failed|crashed`).
    #[serde(default)]
    pub phase: String,
    /// Last-observed timestamp in epoch-milliseconds — drives freshness /
    /// eviction. Supervisors stamp this at emit time.
    #[serde(default)]
    pub updated_at: u64,
    /// Optional most-recent event label, rendered in `extra` when no
    /// progress tuple is available.
    #[serde(default)]
    pub last_event: String,
}

/// Errors returned by [`ingest_pipe_payload`] when a pipe message cannot be
/// committed to the cache.
///
/// `ForeignSource` is not a hard error from the plugin's POV — it just means
/// the message wasn't for us — but callers (incl. tests) benefit from the
/// distinction vs. malformed JSON.
#[derive(Debug)]
pub enum IngestError {
    /// Pipe `name` did not match [`PLUGIN_NAME`]. Cache untouched.
    ForeignSource,
    /// Payload failed `serde_json` parsing. Cache untouched.
    BadJson(serde_json::Error),
    /// Parsed payload has no `agent_id`; refuse to key on the empty string.
    MissingAgentId,
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::ForeignSource => {
                write!(f, "pipe message name did not match {PLUGIN_NAME}")
            }
            IngestError::BadJson(e) => write!(f, "invalid pipe payload JSON: {e}"),
            IngestError::MissingAgentId => write!(f, "pipe payload missing agent_id"),
        }
    }
}

impl std::error::Error for IngestError {}

/// Root plugin state.
///
/// Holds the ordered map of agent id → latest [`StatusSummary`]. `BTreeMap`
/// (not `HashMap`) gives deterministic iteration order so renders are stable
/// across ticks — matches R2's "ordered → deterministic render" guidance.
#[derive(Debug, Default)]
pub struct Status {
    /// Agent id → latest status summary. Written on host by test helpers and
    /// on wasm by [`ingest_pipe_payload`]; read by the wasm render path
    /// (T-096). The `allow(dead_code)` keeps host-only `cargo check` quiet
    /// until T-096 wires render.
    #[allow(dead_code)]
    pub(crate) cache: BTreeMap<String, StatusSummary>,
    /// Epoch-ms of the last eviction pass (diagnostic — not yet surfaced).
    #[allow(dead_code)]
    pub(crate) last_eviction_at: u64,
    /// Mandatory pipe-ingestion permission state (`PermissionType::ReadCliPipes`).
    ///
    /// Set to `Some(false)` when `PermissionRequestResult` reports a denial
    /// for the pipe request so R3 render can show a warning chip instead of
    /// silently failing. `true` iff the pipe permission was denied (render
    /// keys the "ReadCliPipes denied" warning row on this).
    ///
    /// F-703: this flag is dedicated to the PIPE permission only. The R4
    /// `FullHdAccess` request is tracked separately on [`Self::fs_permission`]
    /// so an optional-fs denial cannot knock the plugin into permission-
    /// denied mode and silence pipe ingestion.
    #[allow(dead_code)]
    pub(crate) permission_denied: bool,
    /// Name of the session currently focused by the client, learned from the
    /// zellij `Event::SessionUpdate` stream (the `is_current_session` flag
    /// on `SessionInfo`). Used by [`chip::fit_chips`] to pin the focused
    /// session's chip to row 1 per R3. `None` until the first update
    /// arrives; render handles that by not pinning anything.
    #[allow(dead_code)]
    pub(crate) focused_session: Option<String>,
    /// Tri-state flag tracking whether `PermissionType::ReadCliPipes` was
    /// granted (R2 pipe ingestion — mandatory).
    ///
    /// - `None` — request pending (permission-result event not yet seen).
    /// - `Some(false)` — user denied → [`Self::permission_denied`] is
    ///   flipped to `true` and render shows the warning row.
    /// - `Some(true)` — granted; pipe ingestion works.
    #[allow(dead_code)]
    pub(crate) pipe_permission: Option<bool>,
    /// Tri-state flag tracking whether `PermissionType::FullHdAccess` was
    /// granted (R4 fallback scanning — optional).
    ///
    /// - `None` — request pending (permission-result event not yet seen).
    /// - `Some(false)` — user denied, or zellij rejected the request.
    /// - `Some(true)` — granted; `Event::Timer` will run the fs scan.
    ///
    /// The timer branch only scans when `Some(true)`. A denial here does
    /// NOT flip [`Self::permission_denied`] — pipe-only operation is a
    /// supported mode per R4's "skip if no fs perm" (F-703).
    #[allow(dead_code)]
    pub(crate) fs_permission: Option<bool>,
    /// Latch set the first time we skip the fs scan for lack of permission,
    /// so wasm-side logging only fires once rather than every tick. Host
    /// tests don't exercise this; it's a wasm-only quality-of-life guard.
    #[allow(dead_code)]
    pub(crate) fs_permission_warned: bool,
    /// FIFO queue of pending `request_permission` calls used to correlate
    /// `PermissionRequestResult` events with the permission they belong to.
    ///
    /// zellij-tile 0.44's `Event::PermissionRequestResult(PermissionStatus)`
    /// carries a single Granted/Denied flag with no indication of WHICH
    /// permission was just resolved. F-703 splits the load-time request
    /// into two separate `request_permission` calls (one for `ReadCliPipes`,
    /// one for `FullHdAccess`) so an optional-fs denial cannot also deny
    /// the mandatory pipe permission. zellij processes the calls in order,
    /// so we pop the head of this queue on each result event and route the
    /// status to [`Self::pipe_permission`] or [`Self::fs_permission`]
    /// accordingly.
    #[allow(dead_code)]
    pub(crate) pending_permissions: std::collections::VecDeque<PendingPermission>,
}

/// Which permission a queued `request_permission` call corresponds to.
///
/// See [`Status::pending_permissions`] for the routing contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingPermission {
    /// Mandatory `ReadCliPipes` request — denial flips `permission_denied`.
    Pipe,
    /// Optional `FullHdAccess` request — denial silently disables fs scan.
    Fs,
}

/// Public, host-testable accessor for the cached focused session name.
///
/// R3 requires the currently-focused session's chip to always be visible.
/// Zellij-tile 0.44 does not expose a synchronous `get_focused_session_name()`
/// shim, so we shadow that behaviour by subscribing to `Event::SessionUpdate`
/// and caching the `SessionInfo` whose `is_current_session == true`. Host
/// tests build a `Status` with an explicit cache and exercise this directly.
impl Status {
    /// Return the currently focused session name, if known.
    pub fn get_focused_session_name(&self) -> Option<&str> {
        self.focused_session.as_deref()
    }
}

/// Parse a pipe payload and upsert it into `cache`.
///
/// `pipe_name` is the `PipeMessage::name` set by the supervisor's
/// `zellij pipe --name <target>`; only messages whose name matches
/// [`PLUGIN_NAME`] are accepted, matching R2's source filter. The comparison
/// is exact — forgiving case here would silently accept typos.
///
/// On successful upsert the agent's existing entry (if any) is replaced
/// wholesale; freshness ordering is the supervisor's responsibility via
/// `updated_at` (supervisors only send monotonically newer snapshots).
#[allow(dead_code)] // consumed by wasm_plugin + tests; host-only builds skip wasm_plugin
pub(crate) fn ingest_pipe_payload(
    cache: &mut BTreeMap<String, StatusSummary>,
    pipe_name: &str,
    payload: &str,
) -> Result<(), IngestError> {
    if pipe_name != PLUGIN_NAME {
        return Err(IngestError::ForeignSource);
    }
    let summary: StatusSummary = serde_json::from_str(payload).map_err(IngestError::BadJson)?;
    if summary.agent_id.is_empty() {
        return Err(IngestError::MissingAgentId);
    }
    cache.insert(summary.agent_id.clone(), summary);
    Ok(())
}

/// Drop cache entries for **terminal** agents whose `updated_at` is older
/// than `now_ms - ttl_ms`.
///
/// Per cavekit-plugin-status R2 the 60-minute TTL only applies to agents
/// that are known to be gone (`done`, `failed`, `killed`, `timeout`,
/// `crashed`). Non-terminal agents (`running`, `idle`, `prompting`,
/// `stalled`, `reviewing`, …) stay in the cache indefinitely until a newer
/// pipe/fs update replaces them — evicting a running agent because no
/// pipe message arrived for an hour would make its chip vanish while the
/// agent is still alive.
///
/// Returns the number of entries removed so the 1 Hz timer handler can
/// decide whether to request a redraw. Using saturating arithmetic keeps the
/// pass safe at process startup when `now_ms < ttl_ms` (nothing evicts).
#[allow(dead_code)] // consumed by wasm_plugin + tests; host-only builds skip wasm_plugin
pub(crate) fn evict_stale(
    cache: &mut BTreeMap<String, StatusSummary>,
    now_ms: u64,
    ttl_ms: u64,
) -> usize {
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let before = cache.len();
    cache.retain(|_, summary| {
        // Retain if phase is non-terminal OR the entry is still young.
        !is_terminal_phase(&summary.phase) || summary.updated_at >= cutoff
    });
    before - cache.len()
}

/// Apply the outcome of a `ReadCliPipes` `PermissionRequestResult` to
/// plugin state (F-703).
///
/// Mandatory permission: a denial flips [`Status::permission_denied`] to
/// `true` so the render path shows the "ReadCliPipes denied" warning row
/// and pipe ingestion becomes a no-op. A grant clears the flag and
/// stamps [`Status::pipe_permission`] with `Some(true)`.
///
/// Split out as a pure helper so host tests can exercise the routing
/// without a wasm runtime — the wasm `PermissionRequestResult` handler
/// just forwards here.
#[allow(dead_code)] // consumed by wasm_plugin + tests; host-only builds skip wasm_plugin
pub(crate) fn apply_pipe_permission_result(status: &mut Status, granted: bool) {
    status.pipe_permission = Some(granted);
    status.permission_denied = !granted;
}

/// Apply the outcome of a `FullHdAccess` `PermissionRequestResult` to
/// plugin state (F-703).
///
/// Optional permission: a denial silently disables the fs-scan branch
/// (timer loop checks `fs_permission == Some(true)`) but does NOT flip
/// [`Status::permission_denied`] — pipe-only operation is a supported
/// mode per R4's "skip if no fs perm".
#[allow(dead_code)] // consumed by wasm_plugin + tests; host-only builds skip wasm_plugin
pub(crate) fn apply_fs_permission_result(status: &mut Status, granted: bool) {
    status.fs_permission = Some(granted);
}

/// Wire-level phase strings considered terminal (agent known to be gone).
/// Matches `ark-types::Phase`'s terminal set (`Done`, `Failed`, `Crashed`,
/// `Killed`, `Timeout`). Kept local so the plugin does not depend on
/// ark-types — the wire format is the contract, not the enum.
fn is_terminal_phase(phase: &str) -> bool {
    matches!(phase, "done" | "failed" | "crashed" | "killed" | "timeout")
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::chip::{CHIP_SEPARATOR_WIDTH, Chip, Severity, build_chip, fit_chips};
    use super::fs_scan::{merge_fs_scan, resolve_state_dir, scan_state_dir};
    use super::{EVICTION_TTL_MS, PendingPermission, Status, evict_stale, ingest_pipe_payload};
    use zellij_tile::prelude::*;

    /// Text `color_range` level indices used by `Text::serialize`. Zellij's
    /// plugin protocol maps these to semantic colours chosen by the user's
    /// theme — `0`=info/cyan, `1`=warn/yellow, `2`=error/red, `3`=success/
    /// green in the current default theme. We use the dedicated helpers
    /// (`success_color_range`, `error_color_range`) where available and fall
    /// back to numeric levels for info/warn.
    const INFO_LEVEL: usize = 0;
    const WARN_LEVEL: usize = 1;

    /// Cadence for freshness ticks — R2's "1s timer if stale".
    const TIMER_INTERVAL_SECS: f64 = 1.0;

    /// Best-effort wall-clock in epoch-ms for eviction bookkeeping. We go via
    /// `SystemTime` because zellij-tile 0.44 does not hand the Timer event a
    /// wall-clock value (just elapsed seconds since arm). On the wasip1
    /// target `SystemTime::now` is backed by the WASI `clock_time_get`
    /// syscall so this is legitimate inside the plugin sandbox.
    fn now_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    impl ZellijPlugin for Status {
        fn load(&mut self, _configuration: std::collections::BTreeMap<String, String>) {
            // F-703: request the two permissions SEPARATELY. zellij-tile
            // 0.44's `PermissionRequestResult` carries a single
            // Granted/Denied flag for the whole batch — so bundling
            // `ReadCliPipes` + `FullHdAccess` in one call means a denied
            // `FullHdAccess` also denies `ReadCliPipes`, knocking the
            // plugin into permission_denied mode even though pipe
            // ingestion is the mandatory path. Splitting into two calls
            // lets the user decline fs-scan without losing pipe
            // ingestion. We push a FIFO marker for each so the handler
            // for the arriving `PermissionRequestResult` events knows
            // which permission each event corresponds to (zellij
            // processes requests in the order we submit them).
            self.pending_permissions.push_back(PendingPermission::Pipe);
            request_permission(&[PermissionType::ReadCliPipes]);
            self.pending_permissions.push_back(PendingPermission::Fs);
            request_permission(&[PermissionType::FullHdAccess]);

            // R1: subscribe to the 1 Hz timer (freshness ticks — R2 uses it
            // to redraw / evict when no pipe message arrived) and permission
            // results so we can react if the user denies the request. R3
            // adds `SessionUpdate` so we can track which session the client
            // is focused on (used to pin that chip to row 1 on render).
            subscribe(&[
                EventType::Timer,
                EventType::PermissionRequestResult,
                EventType::SessionUpdate,
            ]);

            // Arm the first freshness tick. Subsequent ticks re-arm from
            // inside `update` so we keep a steady 1 Hz cadence without
            // relying on zellij to auto-repeat.
            set_timeout(TIMER_INTERVAL_SECS);
        }

        fn update(&mut self, event: Event) -> bool {
            match event {
                Event::Timer(_elapsed) => {
                    let now = now_ms();
                    let evicted = evict_stale(&mut self.cache, now, EVICTION_TTL_MS);
                    self.last_eviction_at = now;

                    // R4: optional filesystem fallback. Skip entirely if the
                    // `FullHdAccess` permission was denied or is still
                    // pending — avoids retrying a denied syscall every tick.
                    let fs_changed = if self.fs_permission == Some(true) {
                        let state_dir = resolve_state_dir(|k| std::env::var(k).ok());
                        if state_dir.as_os_str().is_empty() {
                            false
                        } else {
                            let scanned = scan_state_dir(&state_dir);
                            merge_fs_scan(&mut self.cache, scanned)
                        }
                    } else {
                        if !self.fs_permission_warned && self.fs_permission == Some(false) {
                            // One-shot warn so operators spot missing perm in
                            // the zellij log without 1 Hz spam.
                            eprintln!(
                                "{}: FullHdAccess denied; fs fallback scan disabled",
                                super::PLUGIN_NAME
                            );
                            self.fs_permission_warned = true;
                        }
                        false
                    };

                    // Re-arm for the next 1 Hz tick.
                    set_timeout(TIMER_INTERVAL_SECS);
                    // Redraw only when something actually changed; the 1 Hz
                    // render will otherwise be driven by pipe arrivals (R2:
                    // "redraw triggered on every pipe message").
                    evicted > 0 || fs_changed
                }
                Event::PermissionRequestResult(status) => {
                    // F-703: we issue two separate `request_permission`
                    // calls in `load()` (one for `ReadCliPipes`, one for
                    // `FullHdAccess`) and zellij processes them in order.
                    // Pop the FIFO queue head to know which permission
                    // this result event refers to.
                    //
                    // `PermissionStatus::Granted` is the happy path; any
                    // other variant (present + future) is treated as a
                    // denial. A denial on the mandatory pipe permission
                    // flips `permission_denied` so the render path shows
                    // the warning row; a denial on the optional fs
                    // permission only disables the fs scan branch.
                    let granted = matches!(status, PermissionStatus::Granted);
                    match self.pending_permissions.pop_front() {
                        Some(PendingPermission::Pipe) => {
                            super::apply_pipe_permission_result(self, granted);
                        }
                        Some(PendingPermission::Fs) => {
                            super::apply_fs_permission_result(self, granted);
                        }
                        None => {
                            // Result arrived without a matching outbound
                            // request — shouldn't happen given zellij's
                            // FIFO contract, but be defensive: don't
                            // silently mutate permission state on a
                            // stray event.
                            eprintln!(
                                "{}: PermissionRequestResult with empty pending queue; ignoring",
                                super::PLUGIN_NAME
                            );
                        }
                    }
                    true
                }
                Event::SessionUpdate(session_infos, _resurrectable) => {
                    // R3 "focused-session always visible" pin source. Zellij
                    // emits the full session list; the one with
                    // `is_current_session` is the one the user is focused
                    // on. We cache just the name — chip pinning is string
                    // based so we don't need the rest.
                    let new_focus = session_infos
                        .into_iter()
                        .find(|s| s.is_current_session)
                        .map(|s| s.name);
                    let changed = self.focused_session != new_focus;
                    self.focused_session = new_focus;
                    changed
                }
                _ => false,
            }
        }

        fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
            // R2: payload arrives on stdin of `zellij pipe`, surfaced as
            // `PipeMessage::payload`. `None` means EOF on the pipe — nothing
            // to ingest, but also no reason to redraw.
            let Some(payload) = pipe_message.payload else {
                return false;
            };
            match ingest_pipe_payload(&mut self.cache, &pipe_message.name, &payload) {
                Ok(()) => true, // redraw on every accepted update
                // Foreign / malformed messages are silently dropped; the
                // plugin is not a validation layer for supervisors.
                Err(_) => false,
            }
        }

        fn render(&mut self, _rows: usize, cols: usize) {
            // Permission-denied fast path (T-095 carry-over): render a single
            // warning row pointing users at `ark doctor` rather than a blank
            // bar. We keep this above the chip path so even a stale cache
            // doesn't mask the permission state.
            if self.permission_denied {
                let msg = format!(
                    "⚠ {}: ReadCliPipes denied. Run `ark doctor`.",
                    super::PLUGIN_NAME
                );
                let text = Text::new(&msg).color_range(WARN_LEVEL, ..);
                print_text_with_coordinates(text, 0, 0, Some(cols), Some(1));
                return;
            }

            // Build one chip per cached summary in BTreeMap order (stable /
            // deterministic per R2). `build_chip` is pure — we reuse the
            // same helper host tests exercise.
            let focused = self.focused_session.clone();
            let is_focused_for =
                |name: &str| -> bool { focused.as_deref().map(|s| s == name).unwrap_or(false) };
            let chips: Vec<Chip> = self
                .cache
                .values()
                .map(|s| build_chip(s, is_focused_for(&s.name)))
                .collect();

            let (row1, row2) = fit_chips(chips, cols, focused.as_deref());

            // Row 1
            let (row1_text, row1_ranges) = compose_row(&row1);
            emit_row(row1_text, &row1_ranges, 0, cols);
            // Row 2 (may be empty — still emit to clear prior frame)
            let (row2_text, row2_ranges) = compose_row(&row2);
            emit_row(row2_text, &row2_ranges, 1, cols);
        }
    }

    /// Single coloured range inside a composed row.
    ///
    /// `start`/`end` are character-index bounds in the final row string (not
    /// byte offsets — `Text::color_range` operates on `chars().count()`).
    struct ColorRange {
        start: usize,
        end: usize,
        severity: Severity,
    }

    /// Concatenate chip texts separated by [`CHIP_SEPARATOR_WIDTH`] spaces
    /// and compute per-chip colour ranges.
    fn compose_row(chips: &[Chip]) -> (String, Vec<ColorRange>) {
        let mut out = String::new();
        let mut ranges = Vec::with_capacity(chips.len());
        for (idx, chip) in chips.iter().enumerate() {
            if idx > 0 {
                for _ in 0..CHIP_SEPARATOR_WIDTH {
                    out.push(' ');
                }
            }
            let start = out.chars().count();
            out.push_str(&chip.text);
            let end = out.chars().count();
            ranges.push(ColorRange {
                start,
                end,
                severity: chip.severity,
            });
        }
        (out, ranges)
    }

    /// Apply each [`ColorRange`] via the matching `Text` helper, then emit.
    fn emit_row(row_text: String, ranges: &[ColorRange], y: usize, cols: usize) {
        let mut text = Text::new(&row_text);
        for r in ranges {
            text = match r.severity {
                Severity::Ok => text.success_color_range(r.start..r.end),
                Severity::Danger => text.error_color_range(r.start..r.end),
                Severity::Info => text.color_range(INFO_LEVEL, r.start..r.end),
                Severity::Warn => text.color_range(WARN_LEVEL, r.start..r.end),
            };
        }
        print_text_with_coordinates(text, 0, y, Some(cols), Some(1));
    }

    register_plugin!(Status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(agent_id: &str, updated_at: u64) -> StatusSummary {
        summary_with_phase(agent_id, updated_at, "running")
    }

    fn summary_with_phase(agent_id: &str, updated_at: u64, phase: &str) -> StatusSummary {
        StatusSummary {
            agent_id: agent_id.to_string(),
            name: "auth".to_string(),
            orchestrator: "cavekit".to_string(),
            phase: phase.to_string(),
            updated_at,
            last_event: String::new(),
        }
    }

    fn payload_for(agent_id: &str, updated_at: u64) -> String {
        serde_json::to_string(&summary(agent_id, updated_at)).unwrap()
    }

    #[test]
    fn ingest_valid_json_inserts_into_cache() {
        let mut cache = BTreeMap::new();
        let payload = payload_for("agent-1", 1_000);

        ingest_pipe_payload(&mut cache, PLUGIN_NAME, &payload).expect("valid payload ingests");

        assert_eq!(cache.len(), 1);
        let entry = cache.get("agent-1").expect("agent-1 present");
        assert_eq!(entry.phase, "running");
        assert_eq!(entry.updated_at, 1_000);
    }

    #[test]
    fn ingest_invalid_json_leaves_cache_unchanged() {
        let mut cache = BTreeMap::new();
        cache.insert("agent-1".into(), summary("agent-1", 500));

        let err = ingest_pipe_payload(&mut cache, PLUGIN_NAME, "{ not valid json ")
            .expect_err("malformed payload rejected");
        assert!(matches!(err, IngestError::BadJson(_)));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache["agent-1"].updated_at, 500);
    }

    #[test]
    fn ingest_foreign_source_is_dropped() {
        let mut cache = BTreeMap::new();
        let payload = payload_for("agent-1", 1_000);

        let err = ingest_pipe_payload(&mut cache, "some-other-plugin", &payload)
            .expect_err("foreign source rejected");
        assert!(matches!(err, IngestError::ForeignSource));
        assert!(cache.is_empty());
    }

    #[test]
    fn ingest_missing_agent_id_is_rejected() {
        let mut cache = BTreeMap::new();
        // Valid JSON object, but with no agent_id field → defaults to "".
        let err = ingest_pipe_payload(&mut cache, PLUGIN_NAME, r#"{"name":"auth"}"#)
            .expect_err("empty agent_id rejected");
        assert!(matches!(err, IngestError::MissingAgentId));
        assert!(cache.is_empty());
    }

    #[test]
    fn ingest_foreign_source_leaves_valid_json_cache_untouched() {
        // T-125 / cavekit-plugin-status R3 source-filter: even a
        // perfectly well-formed payload addressed at a different pipe
        // name must NOT mutate the cache. Previous ForeignSource test
        // uses an empty cache; this one pre-seeds the cache so we can
        // assert it is byte-identical after the rejected ingest.
        let mut cache = BTreeMap::new();
        cache.insert("existing".into(), summary("existing", 1_000));
        let before = cache.clone();
        let payload = payload_for("intruder", 9_999);

        let err = ingest_pipe_payload(&mut cache, "some-other-plugin", &payload)
            .expect_err("foreign source must be rejected");
        assert!(matches!(err, IngestError::ForeignSource));
        assert_eq!(cache, before, "cache must be untouched on foreign source");
    }

    #[test]
    fn ingest_upserts_newer_snapshot_for_same_agent() {
        let mut cache = BTreeMap::new();
        ingest_pipe_payload(&mut cache, PLUGIN_NAME, &payload_for("agent-1", 1_000)).unwrap();
        ingest_pipe_payload(&mut cache, PLUGIN_NAME, &payload_for("agent-1", 2_000)).unwrap();

        assert_eq!(cache.len(), 1);
        assert_eq!(cache["agent-1"].updated_at, 2_000);
    }

    #[test]
    fn evict_stale_removes_terminal_entries_older_than_ttl() {
        let mut cache = BTreeMap::new();
        // Terminal + stale → evict.
        cache.insert("old".into(), summary_with_phase("old", 0, "done"));
        // Terminal but fresh → keep.
        cache.insert(
            "fresh".into(),
            summary_with_phase("fresh", EVICTION_TTL_MS, "done"),
        );

        // now = 2 * TTL → "old" is past the cutoff, "fresh" is exactly at it.
        let removed = evict_stale(&mut cache, EVICTION_TTL_MS * 2, EVICTION_TTL_MS);

        assert_eq!(removed, 1);
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("fresh"));
    }

    #[test]
    fn evict_stale_retains_young_entries() {
        let mut cache = BTreeMap::new();
        cache.insert("a".into(), summary("a", 10_000));
        cache.insert("b".into(), summary("b", 10_500));

        // now is within TTL of both entries.
        let removed = evict_stale(&mut cache, 11_000, EVICTION_TTL_MS);

        assert_eq!(removed, 0);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn evict_stale_returns_removed_count_for_terminal_entries() {
        let mut cache = BTreeMap::new();
        cache.insert("a".into(), summary_with_phase("a", 0, "done"));
        cache.insert("b".into(), summary_with_phase("b", 0, "failed"));
        cache.insert("c".into(), summary_with_phase("c", 0, "crashed"));
        cache.insert(
            "d".into(),
            summary_with_phase("d", EVICTION_TTL_MS * 10, "done"),
        );

        let removed = evict_stale(&mut cache, EVICTION_TTL_MS * 5, EVICTION_TTL_MS);

        assert_eq!(removed, 3);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key("d"));
    }

    #[test]
    fn evict_stale_is_safe_at_process_startup() {
        // now_ms < ttl_ms — saturating_sub pins the cutoff at 0, so nothing evicts.
        let mut cache = BTreeMap::new();
        cache.insert("a".into(), summary("a", 0));

        let removed = evict_stale(&mut cache, 42, EVICTION_TTL_MS);

        assert_eq!(removed, 0);
        assert_eq!(cache.len(), 1);
    }

    // ---- F-603: non-terminal entries must survive the TTL pass -------------

    #[test]
    fn evict_stale_retains_non_terminal_stale_entry() {
        // A running agent whose supervisor stopped emitting pipe updates
        // for well over an hour must NOT be evicted — the chip should
        // remain visible until a newer snapshot replaces it.
        let mut cache = BTreeMap::new();
        for phase in ["running", "idle", "prompting", "stalled", "reviewing"] {
            cache.insert(phase.to_string(), summary_with_phase(phase, 0, phase));
        }

        let removed = evict_stale(&mut cache, EVICTION_TTL_MS * 100, EVICTION_TTL_MS);

        assert_eq!(removed, 0, "non-terminal entries must not be evicted");
        assert_eq!(cache.len(), 5);
    }

    #[test]
    fn evict_stale_evicts_terminal_stale_entry() {
        // Each terminal phase should be eligible for eviction once stale.
        for phase in ["done", "failed", "crashed", "killed", "timeout"] {
            let mut cache = BTreeMap::new();
            cache.insert("x".into(), summary_with_phase("x", 0, phase));
            let removed = evict_stale(&mut cache, EVICTION_TTL_MS * 2, EVICTION_TTL_MS);
            assert_eq!(
                removed, 1,
                "terminal phase {phase} must be evicted when stale"
            );
            assert!(cache.is_empty());
        }
    }

    // ---- F-703: split pipe/fs permission routing --------------------------

    #[test]
    fn apply_pipe_permission_grant_clears_denied_flag() {
        // Grant of the mandatory `ReadCliPipes` permission must:
        //   - stamp `pipe_permission = Some(true)`
        //   - leave `permission_denied = false` so render does not show
        //     the warning row and pipe ingestion runs.
        let mut status = Status::default();
        apply_pipe_permission_result(&mut status, true);
        assert_eq!(status.pipe_permission, Some(true));
        assert!(!status.permission_denied);
    }

    #[test]
    fn apply_pipe_permission_denial_flips_permission_denied() {
        // Denial of the mandatory `ReadCliPipes` permission must flip
        // `permission_denied` so render surfaces the warning row. Pipe
        // ingestion is dead without this permission, so the plugin
        // intentionally fails loud.
        let mut status = Status::default();
        apply_pipe_permission_result(&mut status, false);
        assert_eq!(status.pipe_permission, Some(false));
        assert!(status.permission_denied);
    }

    #[test]
    fn apply_fs_permission_denial_does_not_flip_permission_denied() {
        // F-703 contract: denial of the OPTIONAL `FullHdAccess`
        // permission must NOT flip `permission_denied`. Pipe ingestion
        // is orthogonal — losing fs fallback only disables the fs-scan
        // branch of the timer loop. If this test regresses, a user who
        // clicks "deny" on the fs prompt loses pipe ingestion too.
        let mut status = Status::default();
        // Simulate the happy-path pipe grant that arrives first.
        apply_pipe_permission_result(&mut status, true);
        apply_fs_permission_result(&mut status, false);
        assert_eq!(status.fs_permission, Some(false));
        assert_eq!(
            status.pipe_permission,
            Some(true),
            "pipe permission untouched by fs result"
        );
        assert!(
            !status.permission_denied,
            "fs denial must NOT mark the plugin permission-denied"
        );
    }

    #[test]
    fn apply_fs_permission_grant_enables_fs_scan_flag() {
        // Grant of the optional `FullHdAccess` permission stamps
        // `fs_permission = Some(true)`; the wasm timer branch gates
        // on exactly this value before running `scan_state_dir`.
        let mut status = Status::default();
        apply_pipe_permission_result(&mut status, true);
        apply_fs_permission_result(&mut status, true);
        assert_eq!(status.fs_permission, Some(true));
        assert_eq!(status.pipe_permission, Some(true));
        assert!(!status.permission_denied);
    }

    #[test]
    fn pipe_granted_fs_denied_allows_pipe_ingestion() {
        // End-to-end host simulation: mandatory pipe granted + optional
        // fs denied → plugin must still accept pipe payloads (render's
        // `permission_denied` warning-row early return is the gate).
        let mut status = Status::default();
        apply_pipe_permission_result(&mut status, true);
        apply_fs_permission_result(&mut status, false);
        assert!(!status.permission_denied);

        // Pipe ingestion works — the real proof that "pipe still works
        // even when fs was denied".
        let payload = payload_for("agent-1", 1_000);
        ingest_pipe_payload(&mut status.cache, PLUGIN_NAME, &payload)
            .expect("pipe ingestion works when pipe permission granted");
        assert_eq!(status.cache.len(), 1);
        assert!(status.cache.contains_key("agent-1"));
    }

    #[test]
    fn pipe_denied_marks_plugin_denied_regardless_of_fs() {
        // Denial on the mandatory pipe permission MUST mark the plugin
        // permission-denied — that's the signal the render path keys on
        // to show the "ReadCliPipes denied" warning row. A later fs
        // grant cannot un-deny the plugin.
        let mut status = Status::default();
        apply_pipe_permission_result(&mut status, false);
        assert!(status.permission_denied);
        apply_fs_permission_result(&mut status, true);
        assert!(
            status.permission_denied,
            "fs grant must not override pipe denial"
        );
        assert_eq!(status.pipe_permission, Some(false));
        assert_eq!(status.fs_permission, Some(true));
    }

    #[test]
    fn evict_stale_mixed_entries_only_terminal_evicted() {
        // Mixed cache: one stale terminal, one stale non-terminal, one
        // fresh terminal, one fresh non-terminal. Only the stale terminal
        // should disappear.
        let mut cache = BTreeMap::new();
        cache.insert(
            "stale-done".into(),
            summary_with_phase("stale-done", 0, "done"),
        );
        cache.insert(
            "stale-running".into(),
            summary_with_phase("stale-running", 0, "running"),
        );
        cache.insert(
            "fresh-done".into(),
            summary_with_phase("fresh-done", EVICTION_TTL_MS * 5, "done"),
        );
        cache.insert(
            "fresh-running".into(),
            summary_with_phase("fresh-running", EVICTION_TTL_MS * 5, "running"),
        );

        let removed = evict_stale(&mut cache, EVICTION_TTL_MS * 2, EVICTION_TTL_MS);

        assert_eq!(removed, 1);
        assert!(!cache.contains_key("stale-done"));
        assert!(cache.contains_key("stale-running"));
        assert!(cache.contains_key("fresh-done"));
        assert!(cache.contains_key("fresh-running"));
    }
}
