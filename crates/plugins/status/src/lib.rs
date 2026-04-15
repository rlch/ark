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
    /// Set when `PermissionRequestResult` reports a denial so R3 render can
    /// show a warning chip instead of silently failing.
    #[allow(dead_code)]
    pub(crate) permission_denied: bool,
    /// Name of the session currently focused by the client, learned from the
    /// zellij `Event::SessionUpdate` stream (the `is_current_session` flag
    /// on `SessionInfo`). Used by [`chip::fit_chips`] to pin the focused
    /// session's chip to row 1 per R3. `None` until the first update
    /// arrives; render handles that by not pinning anything.
    #[allow(dead_code)]
    pub(crate) focused_session: Option<String>,
    /// Tri-state flag tracking whether `PermissionType::FullHdAccess` was
    /// granted (R4 fallback scanning).
    ///
    /// - `None` — request pending (permission-result event not yet seen).
    /// - `Some(false)` — user denied, or zellij rejected the request.
    /// - `Some(true)` — granted; `Event::Timer` will run the fs scan.
    ///
    /// The timer branch only scans when `Some(true)`. This keeps the plugin
    /// from retrying (and logging) on every 1 Hz tick when the user has
    /// declined filesystem access — pipe-only operation is a supported mode
    /// per R4's "skip if no fs perm".
    #[allow(dead_code)]
    pub(crate) fs_permission: Option<bool>,
    /// Latch set the first time we skip the fs scan for lack of permission,
    /// so wasm-side logging only fires once rather than every tick. Host
    /// tests don't exercise this; it's a wasm-only quality-of-life guard.
    #[allow(dead_code)]
    pub(crate) fs_permission_warned: bool,
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
    use super::{EVICTION_TTL_MS, Status, evict_stale, ingest_pipe_payload};
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
            // R1: request the pipe-ingestion permission. R4 optionally adds
            // `FullHdAccess` for the `$XDG_STATE_HOME/ark/agents/*/status.json`
            // fallback scan — requesting it here is best-effort; the plugin
            // still works as pipe-only if the user denies it.
            // Both grants/denials surface via
            // `EventType::PermissionRequestResult`.
            request_permission(&[PermissionType::ReadCliPipes, PermissionType::FullHdAccess]);

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
                    // `PermissionStatus::Granted` is the happy path; any
                    // other variant (present + future) is treated as a
                    // denial so render can surface a warning. The same grant
                    // state also gates the R4 fs fallback scan — zellij
                    // reports a single status for the whole request batch,
                    // so granting implies both `ReadCliPipes` and
                    // `FullHdAccess` are available.
                    let granted = matches!(status, PermissionStatus::Granted);
                    self.permission_denied = !granted;
                    self.fs_permission = Some(granted);
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
