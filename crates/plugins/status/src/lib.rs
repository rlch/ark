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

/// Drop cache entries whose `updated_at` is older than `now_ms - ttl_ms`.
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
    cache.retain(|_, summary| summary.updated_at >= cutoff);
    before - cache.len()
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::{EVICTION_TTL_MS, Status, evict_stale, ingest_pipe_payload};
    use zellij_tile::prelude::*;

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
            // R1: request only the minimal permission the plugin needs — read
            // incoming `zellij pipe` payloads from supervisors. Granted
            // asynchronously; the result arrives via
            // `EventType::PermissionRequestResult`.
            request_permission(&[PermissionType::ReadCliPipes]);

            // R1: subscribe to the 1 Hz timer (freshness ticks — R2 uses it
            // to redraw / evict when no pipe message arrived) and permission
            // results so we can react if the user denies the request.
            subscribe(&[EventType::Timer, EventType::PermissionRequestResult]);

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
                    // Re-arm for the next 1 Hz tick.
                    set_timeout(TIMER_INTERVAL_SECS);
                    // Redraw only when something actually changed; the 1 Hz
                    // render will otherwise be driven by pipe arrivals (R2:
                    // "redraw triggered on every pipe message").
                    evicted > 0
                }
                Event::PermissionRequestResult(status) => {
                    // `PermissionStatus::Granted` is the happy path; any
                    // other variant (present + future) is treated as a
                    // denial so render can surface a warning.
                    self.permission_denied = !matches!(status, PermissionStatus::Granted);
                    true
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

        fn render(&mut self, _rows: usize, _cols: usize) {
            // R3 render stub — filled in by T-096.
        }
    }

    register_plugin!(Status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(agent_id: &str, updated_at: u64) -> StatusSummary {
        StatusSummary {
            agent_id: agent_id.to_string(),
            name: "auth".to_string(),
            orchestrator: "cavekit".to_string(),
            phase: "running".to_string(),
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
    fn evict_stale_removes_entries_older_than_ttl() {
        let mut cache = BTreeMap::new();
        cache.insert("old".into(), summary("old", 0));
        cache.insert("fresh".into(), summary("fresh", EVICTION_TTL_MS));

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
    fn evict_stale_returns_removed_count() {
        let mut cache = BTreeMap::new();
        cache.insert("a".into(), summary("a", 0));
        cache.insert("b".into(), summary("b", 0));
        cache.insert("c".into(), summary("c", 0));
        cache.insert("d".into(), summary("d", EVICTION_TTL_MS * 10));

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
}
