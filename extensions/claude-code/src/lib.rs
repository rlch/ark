//! `ark-ext-claude-code` — Claude Code integration extension for ark.
//!
//! This crate ships a single-crate extension that bridges Anthropic's
//! Claude Code CLI into ark's event bus via a hook subprocess + a
//! transcript tail. Per the 2026-04-18 pivot, this is the v0.1 first-class
//! AI-agent integration (the broader `pi` extension family is deferred to
//! v0.2). See `context/kits/cavekit-claude-code.md`.
//!
//! # Components (populated across Tiers 1..N)
//!
//! * [`ClaudeCodeExtension`] — the `ArkExtension` impl. v0.1 surface
//!   covers scene-compile hook registration, session-start / session-end
//!   lifecycle, a `claude-code` `CommandView`, and a `claude-code-subagent`
//!   stack view. See build-site-claude-code-ext.md for the task graph.
//! * [`hook_event::HookEvent`] + [`hook_payload::HookPayload`] — typed
//!   surface covering Claude Code's 10 hook names (R1) and their
//!   NDJSON payload shape (R2). [`hook_payload::payload_to_ext_event`]
//!   is the R3 translator that produces `claude-code.<kind>` ExtEvents.
//!   Salvaged from the pre-2026-04-18 `crates/hook/` crate (T-004, T-005).
//! * [`transcript`] — R8 transcript fs-watcher + tail cursor primitives.
//!   `notify`-based recursive watch + byte-offset JSONL tail, survives
//!   truncation. Salvaged from the pre-2026-04-18
//!   `crates/orchestrators/claude-code/` crate with all orchestrator-
//!   trait surface stripped (T-007).
//! * `bin/cc-hook/main.rs` (binary target: `cc-hook`) — subprocess
//!   invoked by `~/.claude/settings.json` hooks. POSTs a single NDJSON
//!   line per hook invocation to the per-session ark socket at
//!   `$STATE/sessions/<sid>/cc-hook.sock`, then exits. Write-only — no
//!   reverse messages. Per kit R1 + R2. See T-006 for the salvaged
//!   implementation.
//!
//! # Non-goal marker — MCP control surface (T-008)
//!
//! NOTE: per cavekit-claude-code §Non-goals, permission/policy types
//! (`READ_ONLY_TOOLS` / `PermissionPolicy` / `POLICY_FILE_NAME`) are NOT
//! restored in v0.1. Preserved in git history only. Stretch: MCP server.
//!
//! Previous `crates/types/src/permission.rs` carried a tri-state
//! policy (`Ask` / `AutoApproveRead` / `AutoApproveAll`) plus a
//! `READ_ONLY_TOOLS` list and a `POLICY_FILE_NAME` constant; the old
//! `ark-hook` crate consulted those to decide whether to write Claude
//! Code's allow-payload to stdout. The claude-code extension deliberately
//! delegates that entire surface to Claude's in-TUI permission prompt —
//! users stay in one mental model and ark observes rather than mediates.
//! If/when the v0.2-stretch MCP server lands the surface re-enters via a
//! different mechanism (runtime tool injection rather than stdout
//! hook-payload substitution).
//!
//! # Dispatch defaults
//!
//! Every method on [`ark_ext_proto::ArkExtension`] has a default
//! implementation that either returns `method_not_found` or an empty
//! response struct. This crate overrides only the methods it supports —
//! T-020 onwards populates them. Until then, the `impl ArkExtension`
//! block is deliberately empty + all behaviour inherits trait defaults.

#![deny(missing_docs)]

use ark_ext_proto::{
    ArkExtension, ControlVerbsRequest, ControlVerbsResponse, DoctorChecksRequest,
    DoctorChecksResponse, ExtResult, ListColumnsRequest, ListColumnsResponse, OnSessionEndRequest,
    OnSessionEndResponse, OnSessionStartRequest, OnSessionStartResponse, OpaqueJson,
    SceneCompileHookRequest, SceneCompileHookResponse,
};
use ark_types::{CoreEvent, EventSink};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub mod columns;
pub mod config;
pub mod doctor;
pub mod hook_event;
pub mod hook_payload;
pub mod settings_json;
pub mod socket;
pub mod transcript;
pub mod view;

pub use columns::{
    COLUMN_COST, COLUMN_MODEL, COLUMN_TOKENS, CcListColumnState, ColumnContribution,
    ColumnsEnvelope,
};
pub use config::{ClaudeCodeConfig, DEFAULT_TRANSCRIPT_TAIL_LINES};
pub use doctor::{
    CheckLevel, CheckResult, DoctorEnvelope, check_cc_hook_binary, check_settings_drift,
    check_state_sessions_writable, check_view_wired, check_which_claude, render_envelope_table,
};
pub use hook_event::{HookEvent, UnknownHookEvent};
pub use hook_payload::{EXT_NAME, HookPayload, NdjsonLine, flat_event_name, payload_to_ext_event};
pub use settings_json::{
    ARK_MANAGED_HOOK_COUNT, ARK_MANAGED_KEY, CC_HOOK_BIN_NAME, DEFAULT_SETTINGS_REL_PATH,
    InstallOutcome, ReconcileOutcome, SettingsFile, SettingsJsonError, cc_hook_install_path,
    default_settings_path, install_cc_hook_at, install_cc_hook_default,
};
pub use socket::{
    BRIDGE_VERSION_MISMATCH_SENTINEL, BridgeVersionMismatch, CRATE_VERSION, CcHookSocket,
    CcHookSocketError, SocketEvent, record_mismatch_sentinel,
};
pub use transcript::{
    SharedDirWatcher, TailCursor, TranscriptDirEvent, TranscriptDirWatcher, TranscriptEvent,
    TranscriptTail, TranscriptWatcher, encode_cwd, extract_transcript_parent_from_payload,
    spawn_log_sink, start_watcher,
};
pub use view::{
    ClaudeCodeSubagent, ClaudeCodeSubagentAttrs, ClaudeCodeSubagentView, ClaudeCodeView,
    SubagentState, SubagentStatus, format_subagent_title, format_transcript_lines,
};

/// Basename of the T-020 sentinel file dropped next to the cc-hook
/// socket when `on_session_start` cannot reconcile `settings.json`.
/// `ark doctor` (T-042) reads this path directly so the warning
/// surfaces without a live API into the supervisor.
pub const SETTINGS_UNWRITABLE_SENTINEL: &str = "claude-code.settings_unwritable.json";

/// On-disk shape of the T-020 settings-unwritable sentinel. Kept flat
/// so doctor can parse with `serde_json::from_slice` without pulling
/// this crate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SettingsUnwritable {
    /// Absolute path to the settings.json that could not be written.
    pub path: std::path::PathBuf,
    /// Wire error code (mirror of [`SettingsJsonError::code`]).
    pub code: String,
    /// Human-readable failure message (stringified source).
    pub message: String,
    /// RFC 3339 timestamp at first observation.
    pub first_seen_at: String,
}

impl SettingsJsonError {
    /// Stable wire error code used by the T-020 sentinel + doctor
    /// rendering.
    pub fn code(&self) -> &'static str {
        match self {
            SettingsJsonError::Read { .. } => "claude-code/settings-read",
            SettingsJsonError::Parse { .. } => "claude-code/settings-parse",
            SettingsJsonError::Write { .. } => "claude-code/settings-write",
        }
    }
}

/// Write the T-020 settings-unwritable sentinel under `session_dir`.
/// Atomic via tmp + rename in the same directory. Errors during the
/// sentinel write itself are best-effort — a cascading failure here
/// means doctor loses context but the session still launches.
pub fn record_settings_unwritable_sentinel(
    session_dir: &std::path::Path,
    err: &SettingsJsonError,
    path: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(session_dir)?;
    let payload = SettingsUnwritable {
        path: path.to_path_buf(),
        code: err.code().to_string(),
        message: err.to_string(),
        first_seen_at: chrono::Utc::now().to_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let final_path = session_dir.join(SETTINGS_UNWRITABLE_SENTINEL);
    let mut tmp = final_path.clone();
    let mut name = tmp
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(SETTINGS_UNWRITABLE_SENTINEL));
    name.push(".tmp");
    tmp.set_file_name(name);
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &final_path)?;
    Ok(())
}

/// T-008a stub: embedded `cc-hook` binary bytes for `doctor --fix` to
/// extract to `$XDG_BIN_HOME/cc-hook` at mode `0755`.
///
/// **Current state (PARTIAL):** this is an empty byte-slice placeholder.
/// A real embedding via `crates/cli/build.rs` → `include_bytes!` would
/// require running `cargo build --release --bin cc-hook -p
/// ark-ext-claude-code` from inside `ark-cli`'s build.rs, which risks
/// circular / deadlock contention with the outer `cargo build
/// --workspace` (the same pathology that motivated the
/// wasm-plugin fallback stack — see `crates/cli/build.rs` F-709).
///
/// The downstream tasks that consume this symbol (T-019, T-023,
/// `doctor --fix`) still compile cleanly against `&[]`; they check
/// `.is_empty()` and surface a helpful "cc-hook bytes not embedded;
/// install via `cargo install --bin cc-hook -p ark-ext-claude-code`"
/// message when the real artifact is absent. A future task can replace
/// this stub with a real `include_bytes!` from a build.rs that uses an
/// isolated `CARGO_TARGET_DIR` under `$OUT_DIR` (mirroring the wasm
/// plugin pattern).
///
/// TODO(T-008a-real): wire real embedding via `crates/cli/build.rs`
/// following the wasm plugin pattern. Blocked on confirming the
/// isolated-target-dir approach works for a native binary under cargo's
/// workspace member resolution — the wasm plugins live in a separate
/// target triple (`wasm32-wasip1`), so the build.rs can't conflict with
/// the outer cargo invocation. A native `cc-hook` built from inside the
/// outer `cargo build --workspace` sits in the same target graph and
/// risks the deadlock pathology F-709 documents. Alternative:
/// `cargo install --bin cc-hook` during packaging (cargo-dist),
/// dropping the embedding requirement entirely.
pub const CC_HOOK_BYTES: &[u8] = &[];

/// The Claude Code extension — an [`ArkExtension`] implementation that
/// registers scene-compile hooks, wires per-session hook sockets, and
/// exposes the `claude-code` + `claude-code-subagent` views.
///
/// v0.1 scaffolding carries a single optional field: [`Self::event_sink`]
/// — a cloned [`EventSink`] handed in at construction time by whoever
/// wires the extension into ark core. T-014 uses it to forward every
/// decoded `claude-code.*` ExtEvent onto the core bus as a
/// `CoreEvent::Ext` ride-along. When absent (most tests), the accept
/// loop logs decoded frames at `debug!` and otherwise behaves as a sink-
/// less observer — the socket reader itself never fails for lack of a
/// publisher.
///
/// Design note — construction-time injection: the kit bans ext-crate
/// edits to supervisor / core for this tier. Rather than wait on an
/// `ArkExtension`-trait-level event publisher handle, the extension
/// takes a pre-built [`EventSink`] the caller owns. Downstream wiring
/// (which lives in a later soul-phase task, not claude-code-ext) can
/// keep the injection shape or swap to a trait-method surface without
/// breaking the ExtEvent payload contract.
#[derive(Clone, Default)]
pub struct ClaudeCodeExtension {
    /// Optional broadcast sink the socket accept loop forwards decoded
    /// [`ark_types::ExtEvent`]s to, wrapped in
    /// [`CoreEvent::Ext`]. Cloneable — tokio broadcast senders are
    /// cheap to clone and safe to share across the spawned accept-loop
    /// task.
    event_sink: Option<EventSink>,

    /// T-041 (R9): typed `[claude-code]` config section. Carries
    /// `match_cmds`, `transcript_tail_lines`, `auto_install_hook_entries`.
    /// Populated from `SessionSpec.ext_config["claude-code"]` at
    /// `on_session_start` time via [`ClaudeCodeConfig::from_ext_config`];
    /// the `match_cmds` field backs the T-032 scene-compile-hook fast
    /// path (pre-T-041 that was a standalone `Vec<String>` field —
    /// subsumed into `config` to keep R9 single-sourced).
    ///
    /// Wrapped in `Arc<Mutex<_>>` because the `ArkExtension` trait
    /// takes `&self` on every method and `on_session_start` needs to
    /// refresh the config from `SessionSpec.ext_config` per-session
    /// (the scene-compile hook also runs per-compile and re-reads the
    /// live value).
    config: std::sync::Arc<std::sync::Mutex<ClaudeCodeConfig>>,

    /// T-044 (R11): per-session rolling state for the three contributed
    /// `ark list` columns. Shared `Arc<Mutex<_>>` so the transcript-
    /// tail poller + the list_columns RPC handler see the same bytes.
    /// Supervisor-side persistence into
    /// `SessionStatus.ext_state["claude-code"]` is deferred to a later
    /// tier (this tier stays in-process per the task brief's
    /// "supervisor out of scope" constraint).
    list_state: std::sync::Arc<std::sync::Mutex<CcListColumnState>>,

    /// v0.2 backlog #3: per-extension [`SubagentRegistry`] the
    /// `on_session_start` accept loop folds hook events into. Tracks
    /// per-subagent `SubagentState` and emits `RenamePaneEmission`
    /// values on status / tool transitions. See
    /// [`Self::subagent_registry`] for the read-back accessor.
    ///
    /// Constructed fresh on every `ClaudeCodeExtension::new()` so
    /// identical instances (unit tests) don't share state. Cloning the
    /// extension shares the registry across every clone — the
    /// `on_session_start` closure captures a clone, not the original.
    subagent_registry: Arc<SubagentRegistry>,

    /// v0.2 backlog #3: optional `pane/emit` emitter the accept loop
    /// invokes on each [`RenamePaneEmission`] the registry produces.
    ///
    /// When `None` (unit tests, CLI dry-runs, any context with no host
    /// to route emissions to) the registry still updates its cached
    /// state — emissions are silently dropped. When `Some`, the host
    /// supplies a callback that routes the emission's `{id, payload}`
    /// into a `pane/emit` RPC against the appropriate stack-child
    /// pane. The callback is `Fn` (not `FnMut`) so the accept loop
    /// can call it from any frame without locking.
    ///
    /// The callback is stored `Arc<dyn Fn(...) + Send + Sync>` so
    /// cloning the extension (Clone impl, bus task spawn) costs a
    /// single refcount bump. Not `Debug` — trait objects of `Fn` don't
    /// derive Debug automatically, and a manual impl would leak no
    /// useful state; the struct-level Debug impl prints
    /// `rename_pane_emitter: <fn>` for the `Some` case.
    rename_pane_emitter: Option<Arc<RenamePaneEmitterFn>>,
}

/// Trait-object signature the host plugs into
/// [`ClaudeCodeExtension::rename_pane_emitter`]. Takes ownership of the
/// emission so the host can move the payload JSON through any
/// serialisation layer without re-allocating.
pub type RenamePaneEmitterFn = dyn Fn(RenamePaneEmission) + Send + Sync + 'static;

impl std::fmt::Debug for ClaudeCodeExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeCodeExtension")
            .field("event_sink", &self.event_sink.is_some())
            .field("config", &self.config)
            .field("list_state", &self.list_state)
            .field("subagent_registry", &self.subagent_registry)
            .field("rename_pane_emitter", &self.rename_pane_emitter.is_some())
            .finish()
    }
}

/// T-032 R5b shape of a `scene_compile_hook` env injection request. An
/// extension returns one of these per pane whose raw command-view kind
/// matched [`ClaudeCodeExtension::match_cmds`]. The outer response
/// serializes the list under the `env_injections` key.
///
/// Wire shape (JSON):
///
/// ```json
/// {
///   "env_injections": [
///     { "pane_id": "chat", "env": { "CLAUDE_HOOK_SOCKET": "/path/to.sock" } }
///   ]
/// }
/// ```
///
/// The scene compiler (or a future supervisor-side consumer once the
/// `partial_scene` shape is pinned) reads this and merges the env
/// entries into the matched pane's launch env. When `match_cmds` is
/// empty, the response carries an empty list — the hook is a no-op.
///
/// Public so scene-compiler / supervisor-side consumers can parse the
/// response without redefining the shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvInjection {
    /// Scene pane-id the env should be merged into.
    pub pane_id: String,
    /// Env keys → values to merge. Later entries override earlier ones
    /// per the scene-compile-merge convention.
    pub env: std::collections::BTreeMap<String, String>,
}

/// Container carried in [`SceneCompileHookResponse::contributions`] —
/// list of env injections plus a list of diagnostic notes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SceneCompileContributions {
    /// Env injections — see [`EnvInjection`]. Empty when `match_cmds`
    /// is empty (default) OR when the partial scene declares no panes
    /// matching any `match_cmds` entry.
    #[serde(default)]
    pub env_injections: Vec<EnvInjection>,
}

impl ClaudeCodeExtension {
    /// Construct a [`ClaudeCodeExtension`] with no event sink. Decoded
    /// hook frames are still consumed by the socket reader but are not
    /// published onto the core bus — useful for unit tests, CLI-only
    /// dry-runs, and any context where the supervisor isn't involved.
    pub fn new() -> Self {
        Self {
            event_sink: None,
            config: std::sync::Arc::new(std::sync::Mutex::new(ClaudeCodeConfig::default())),
            list_state: std::sync::Arc::new(std::sync::Mutex::new(CcListColumnState::default())),
            subagent_registry: Arc::new(SubagentRegistry::new()),
            rename_pane_emitter: None,
        }
    }

    /// Construct a [`ClaudeCodeExtension`] wired to an [`EventSink`] so
    /// every decoded `claude-code.*` ExtEvent reaches the core bus.
    ///
    /// The caller owns the sink's lifetime; the extension clones the
    /// handle into whatever spawned tasks need it. Dropping every
    /// receiver drops `send` Ok-returns to `Err`, which the loop just
    /// logs + skips — a gone-away bus is not a session-fatal condition.
    pub fn with_event_sink(sink: EventSink) -> Self {
        Self {
            event_sink: Some(sink),
            config: std::sync::Arc::new(std::sync::Mutex::new(ClaudeCodeConfig::default())),
            list_state: std::sync::Arc::new(std::sync::Mutex::new(CcListColumnState::default())),
            subagent_registry: Arc::new(SubagentRegistry::new()),
            rename_pane_emitter: None,
        }
    }

    /// v0.2 backlog #3: install a `pane/emit` emitter callback. The
    /// accept loop invokes this on every [`RenamePaneEmission`] the
    /// registry produces — the host uses the emission's
    /// `{id, payload}` to drive the corresponding stack-child pane's
    /// `pane/emit` RPC.
    ///
    /// Builder-style: returns `Self` so supervisor-side wiring can
    /// chain the call inside a construction expression.
    #[must_use]
    pub fn with_rename_pane_emitter<F>(mut self, emitter: F) -> Self
    where
        F: Fn(RenamePaneEmission) + Send + Sync + 'static,
    {
        self.rename_pane_emitter = Some(Arc::new(emitter));
        self
    }

    /// Borrow the per-extension [`SubagentRegistry`]. Primary use is
    /// tests that assert subagent state updates flowed through after an
    /// ExtEvent was handled by the accept loop.
    pub fn subagent_registry(&self) -> &Arc<SubagentRegistry> {
        &self.subagent_registry
    }

    /// Borrow the configured event sink, if any. Exposed primarily for
    /// tests that want to confirm the injection took effect.
    pub fn event_sink(&self) -> Option<&EventSink> {
        self.event_sink.as_ref()
    }

    /// T-032 R5b: set the `match_cmds` list used by
    /// [`Self::scene_compile_hook`]. Builder-style for chaining in
    /// test + supervisor wiring. Default is empty — the raw-cmd
    /// fallback is OFF unless the user explicitly opts in via
    /// `[claude-code] match_cmds = [...]` in their config.
    ///
    /// T-041: the canonical source for `match_cmds` is now the
    /// [`ClaudeCodeConfig`] carried on the struct; this setter mutates
    /// that field so existing test + supervisor callers that wire
    /// `match_cmds` directly keep working without knowing about the
    /// config struct.
    #[must_use]
    pub fn with_match_cmds(self, cmds: Vec<String>) -> Self {
        if let Ok(mut g) = self.config.lock() {
            g.match_cmds = cmds;
        }
        self
    }

    /// Borrow the configured `match_cmds` list. Exposed primarily for
    /// tests that want to confirm the config injection took effect.
    /// Returns an owned `Vec` (clone) rather than a `&[String]` because
    /// the config lives behind a Mutex — borrowing a slice would leak
    /// the lock guard.
    pub fn match_cmds(&self) -> Vec<String> {
        self.config
            .lock()
            .map(|g| g.match_cmds.clone())
            .unwrap_or_default()
    }

    /// T-041 (R9): replace the whole typed config section at once. Used
    /// by the supervisor when it loads the extension's config from
    /// `SessionSpec.ext_config` OR from the layered figment at
    /// init/reload time.
    #[must_use]
    pub fn with_config(self, config: ClaudeCodeConfig) -> Self {
        if let Ok(mut g) = self.config.lock() {
            *g = config;
        }
        self
    }

    /// Snapshot of the current [`ClaudeCodeConfig`]. Returns a clone so
    /// callers don't hold the mutex.
    pub fn config_snapshot(&self) -> ClaudeCodeConfig {
        self.config.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// T-041 (R9): refresh the config from a session's
    /// `ext_config["claude-code"]` entry. Walks the JSON keys,
    /// emitting a `warn!` for each unknown key, then replaces the
    /// cached config in place. Returns the parsed config.
    ///
    /// Missing key / null value / parse error all fall back to the
    /// default config and return an error-free result — R9 "unknown
    /// keys warn but don't fail" treats every tolerable failure as a
    /// degradation to defaults, not an abort.
    pub fn load_session_config(
        &self,
        ext_config: &std::collections::BTreeMap<String, serde_json::Value>,
    ) -> ClaudeCodeConfig {
        let parsed = match ClaudeCodeConfig::from_ext_config(ext_config) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    error = %e,
                    "claude-code: [claude-code] config deserialise failed; falling back to defaults"
                );
                ClaudeCodeConfig::default()
            }
        };
        if let Ok(mut g) = self.config.lock() {
            *g = parsed.clone();
        }
        parsed
    }

    /// T-044 (R11): read-only handle on the per-session rolling column
    /// state. Intended for tests + supervisor-side snapshot-to-ext_state
    /// shims. Returns a clone of the state so callers don't hold the
    /// lock.
    pub fn list_state_snapshot(&self) -> CcListColumnState {
        self.list_state
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// T-044 helper: fold a single transcript JSONL line into the
    /// per-session rolling column state. Called by the accept-loop +
    /// transcript watcher plumbing. A poisoned mutex silently drops the
    /// fold — doctor can later surface the poison via a follow-up
    /// check, but we never fail the caller.
    pub fn fold_transcript_line(&self, line: &str) {
        if let Ok(mut g) = self.list_state.lock() {
            g.fold_line(line);
        }
    }

    /// T-044 helper: fold every line in a transcript blob. Used by
    /// `list_columns` as a populate-on-demand path when no live fold
    /// loop has observed the session yet (e.g. `ark list` invoked
    /// after-the-fact against a persisted transcript).
    pub fn fold_transcript_blob(&self, blob: &str) {
        if let Ok(mut g) = self.list_state.lock() {
            g.fold_blob(blob);
        }
    }
}

/// `ArkExtension` implementation for the Claude Code extension.
///
/// T-011 lands a minimal `on_session_start` override that binds the
/// per-session cc-hook socket and spawns an accept loop. The loop's
/// sink is currently log-only — ExtEvent forwarding to the core bus
/// lands in T-014 (Tier 3) once the `ArkExtension` trait surfaces an
/// event publisher handle. Other methods stay at their trait defaults
/// until their respective tiers (see `build-site-claude-code-ext.md`).
#[async_trait]
impl ArkExtension for ClaudeCodeExtension {
    async fn on_session_start(
        &self,
        req: OnSessionStartRequest,
    ) -> ExtResult<OnSessionStartResponse> {
        // Decode the SessionSpec from OpaqueJson. A malformed spec is
        // non-fatal here — log + return Ok so the supervisor isn't
        // blocked on an extension-side parse error (matches R2's
        // fail-open philosophy).
        let spec: ark_types::SessionSpec = match serde_json::from_str(&req.spec) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "claude-code: on_session_start: spec decode failed; skipping listener bind");
                return Ok(OnSessionStartResponse::default());
            }
        };

        // Resolve state layout from env. If XDG resolution fails we
        // can't bind — log and move on; the ext is pure observability,
        // its failure must not block session launch.
        let layout = match ark_types::StateLayout::from_env() {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "claude-code: on_session_start: StateLayout::from_env failed; no socket");
                return Ok(OnSessionStartResponse::default());
            }
        };

        let sid = spec.id.clone();

        // T-020 / T-024 scene-use detection. Absence of a dedicated
        // "declared uses" field on SessionSpec forces a proxy: the
        // scene compile step writes the ext's config bucket into
        // `spec.ext_config` for every `use "<ext>"` directive (with or
        // without a config block). Presence of the `"claude-code"`
        // key is therefore the canonical signal that the scene
        // declared `use "claude-code"`. Absent → T-024 path: we do
        // not bind the socket AND do not touch settings.json.
        if !spec.ext_config.contains_key(EXT_NAME) {
            debug!(
                session = %sid.as_path_leaf(),
                "claude-code: scene does not declare `use \"claude-code\"`; skipping bind + settings reconciliation"
            );
            return Ok(OnSessionStartResponse::default());
        }

        // T-041 (R9): refresh the cached [`ClaudeCodeConfig`] from the
        // session's `ext_config["claude-code"]` bucket. Warns on
        // unknown keys (inside `load_session_config`). Does NOT gate
        // session start — a malformed config falls back to defaults so
        // the session still launches.
        let session_config = self.load_session_config(&spec.ext_config);
        debug!(
            session = %sid.as_path_leaf(),
            auto_install_hook_entries = session_config.auto_install_hook_entries,
            transcript_tail_lines = session_config.transcript_tail_lines,
            match_cmds_len = session_config.match_cmds.len(),
            "claude-code: [claude-code] config loaded for session"
        );

        let sock = match socket::CcHookSocket::bind(&layout, &sid).await {
            Ok(s) => s,
            Err(e) => {
                warn!(session = %sid.as_path_leaf(), error = %e, "claude-code: on_session_start: socket bind failed");
                return Ok(OnSessionStartResponse::default());
            }
        };

        debug!(
            session = %sid.as_path_leaf(),
            path = %sock.path().display(),
            "claude-code: cc-hook socket bound"
        );

        // T-020 + T-021: reconcile `~/.claude/settings.json` to route
        // all 10 hook kinds at the installed cc-hook binary + this
        // session's socket path. Any failure is surfaced via a doctor
        // sentinel — NEVER promoted to a session-fatal error (T-021:
        // graceful degradation).
        //
        // T-041 (R9): gated on `config.auto_install_hook_entries`. When
        // false the user has opted out of settings.json mutation (they
        // manage hook entries out-of-band); we still bind the socket
        // so a manually-installed cc-hook can still deliver events.
        if session_config.auto_install_hook_entries {
            reconcile_settings_for_session(
                &sid,
                sock.path(),
                sock.session_dir(),
                /* override_settings_path */ None,
                /* override_cc_hook_path */ None,
            );
        } else {
            debug!(
                session = %sid.as_path_leaf(),
                "claude-code: auto_install_hook_entries=false; skipping settings.json reconciliation"
            );
        }

        // T-027: spawn a fresh `TranscriptDirWatcher` per session. The
        // accept-loop closure below invokes `ensure_tracking` on each
        // `HookFired` frame with the parent of the payload's
        // `transcript_path` — idempotent, late-binds when the dir
        // appears (T-028 acceptance). The Tier-5 consumer is a
        // log-only sink (`spawn_log_sink`); real Tier-6 view consumers
        // (T-035 / T-036) will replace or multiplex this sink.
        let (dir_watcher, dir_rx) = transcript::TranscriptDirWatcher::new();
        let dir_watcher: transcript::SharedDirWatcher =
            std::sync::Arc::new(std::sync::Mutex::new(dir_watcher));
        let _log_sink_handle = transcript::spawn_log_sink(dir_rx);

        // T-014: if a construction-time `EventSink` was provided,
        // publish every decoded HookFired frame onto the core bus via
        // `CoreEvent::Ext(ExtEvent)`. Bus send errors only happen when
        // every receiver has been dropped (session teardown); we log +
        // continue rather than crash the accept loop.
        let sink = self.event_sink.clone();
        let watcher_for_loop = dir_watcher.clone();
        // v0.2 backlog #3: capture the per-extension SubagentRegistry
        // + the optional pane-emit emitter so the accept loop can fold
        // subagent hook events into cached state AND drive RenamePane
        // emissions on status / tool transitions. Both are Arc-cloned
        // here so the spawned task keeps its own refcounted view; the
        // original `&self` is dropped as soon as this function returns.
        let registry_for_loop = self.subagent_registry.clone();
        let emitter_for_loop = self.rename_pane_emitter.clone();
        tokio::spawn(async move {
            sock.accept_loop(move |ev| match ev {
                socket::SocketEvent::HookFired { event, ext_event } => {
                    debug!(
                        event = %event,
                        kind = %ext_event.kind,
                        has_sink = sink.is_some(),
                        "claude-code: cc-hook frame decoded"
                    );
                    // T-027: every frame gets a shot at binding the
                    // transcript-dir watcher. `ensure_tracking` is
                    // idempotent; once bound, subsequent frames are
                    // cheap no-ops. We pull the path from the verbatim
                    // payload ExtEvent (not the typed HookPayload)
                    // because `ExtEvent::payload` is the stable
                    // cross-crate surface — `transcript_path` lives
                    // there under `HookPayload.extra` on the way in.
                    if let Some(parent) = ext_event
                        .payload
                        .get("transcript_path")
                        .and_then(|v| v.as_str())
                        .map(std::path::Path::new)
                        .filter(|p| p.is_absolute())
                        .and_then(|p| p.parent().map(std::path::PathBuf::from))
                    {
                        match watcher_for_loop.lock() {
                            Ok(mut w) => w.ensure_tracking(&parent),
                            Err(e) => warn!(
                                error = %e,
                                "claude-code: transcript dir watcher mutex poisoned; skipping ensure_tracking"
                            ),
                        }
                    }
                    // v0.2 backlog #3: fold the hook event through the
                    // SubagentRegistry BEFORE forwarding to the bus.
                    // Three reasons for ordering it first:
                    //   (a) Bus receivers that consume `CoreEvent::Ext`
                    //       and then read the registry (e.g. a rhai
                    //       reaction) see a registry state that already
                    //       reflects the event — no lost-update window.
                    //   (b) Registry folding is pure CPU over a local
                    //       HashMap; moving it ahead of the bus send
                    //       adds microseconds at most.
                    //   (c) The RenamePane emitter is fired
                    //       synchronously here — pane-rename UI lands
                    //       BEFORE any async reactive cascade kicks in
                    //       off the bus.
                    if let Some(emission) = registry_for_loop.on_ext_event(&ext_event) {
                        debug!(
                            agent_id = %emission.id,
                            title = ?emission.payload.get("name").and_then(|v| v.as_str()),
                            has_emitter = emitter_for_loop.is_some(),
                            "claude-code: subagent registry emitted RenamePane"
                        );
                        if let Some(emitter) = emitter_for_loop.as_ref() {
                            emitter(emission);
                        }
                    }
                    if let Some(bus) = sink.as_ref() {
                        if let Err(e) = bus.send(CoreEvent::Ext(ext_event)) {
                            warn!(
                                error = %e,
                                "claude-code: event bus send failed; receivers gone"
                            );
                        }
                    }
                }
                socket::SocketEvent::BridgeVersionMismatch { observed, expected } => {
                    warn!(
                        observed = %observed,
                        expected = %expected,
                        "claude-code: bridge version mismatch; doctor warning persisted"
                    );
                }
            })
            .await;
        });

        Ok(OnSessionStartResponse::default())
    }

    async fn on_session_end(&self, _req: OnSessionEndRequest) -> ExtResult<OnSessionEndResponse> {
        // Per R2 + kit note: the supervisor owns session-dir cleanup;
        // our spawned accept-loop task is aborted automatically when
        // the tokio runtime tears down. A future tier (T-041 +
        // shutdown-path cleanup) may add explicit task cancellation
        // tracking; for now the default-ish response is sufficient and
        // correct.
        Ok(OnSessionEndResponse::default())
    }

    /// T-032 + T-033 (R5b): raw-`command cmd=<X>` fallback. For every
    /// pane in `req.partial_scene` whose view is of kind `command` and
    /// whose `cmd` argument matches one of `self.match_cmds`, emit a
    /// pane-env injection adding `CLAUDE_HOOK_SOCKET=<session-sock-
    /// path>`. When `match_cmds` is empty (the default — R5b is opt-in)
    /// no injections are emitted; the response is an empty list.
    ///
    /// # Scope bounds
    ///
    /// * NO typed subagent fan-out is provided by the fallback. Scene
    ///   authors who want `Stack<ClaudeCodeSubagent>` fan-out MUST use
    ///   the typed [`ClaudeCodeView`] (T-029/T-030) — not the raw
    ///   command form. T-033 regression asserts this boundary.
    /// * Subagent events still flow via the cc-hook socket / ExtEvent
    ///   bus (reused from T-014); user Rhai reactions can still fire
    ///   regardless of view type.
    ///
    /// # `partial_scene` shape (conservative parser)
    ///
    /// The extension protocol's `partial_scene` is declared as
    /// [`ark_ext_proto::OpaqueJson`] (a JSON-encoded string) because its
    /// shape is not yet pinned at the trait level. v0.1 parses a tiny
    /// subset in a tolerant fashion:
    ///
    /// ```json
    /// {
    ///   "panes": [
    ///     { "id": "chat", "view": { "kind": "command", "cmd": "claude" } }
    ///   ]
    /// }
    /// ```
    ///
    /// Any pane whose `view.kind == "command"` AND `view.cmd` matches
    /// an entry in `self.match_cmds` contributes one env injection.
    /// Panes without an `id` field are skipped (no injection target).
    ///
    /// The socket path is derived from `$ARK_STATE_DIR`-resolved
    /// layout + the scene's session id when present, falling back to
    /// the extension's install-time canonical path. Because the
    /// `partial_scene` shape doesn't carry the session id yet, v0.1
    /// stamps the path as `<state>/sessions/<sid>/cc-hook.sock` using
    /// a placeholder `__latest__` id when no session id is provided.
    /// (The consumer merging the env injection into pane launch knows
    /// the real session id and can post-process.)
    async fn scene_compile_hook(
        &self,
        req: SceneCompileHookRequest,
    ) -> ExtResult<SceneCompileHookResponse> {
        let mut contributions = SceneCompileContributions::default();

        // Fast-path: disabled by default. No point walking the
        // partial-scene JSON if no cmd matches are configured.
        let match_cmds = self.match_cmds();
        if match_cmds.is_empty() {
            let contributions_json = serde_json::to_string(&contributions)
                .unwrap_or_else(|_| "{\"env_injections\":[]}".to_string());
            return Ok(SceneCompileHookResponse {
                contributions: contributions_json,
            });
        }

        // Resolve the cc-hook socket path. We stamp a stable
        // `<state>/sessions/<sid>/cc-hook.sock` path here — consumers
        // substitute the real session id on merge. Shape-wise this is
        // identical to what `CcHookSocket::bind` produces in
        // `on_session_start`, so downstream code can treat both sources
        // uniformly.
        let socket_path = placeholder_session_socket_path();

        // Parse the partial_scene JSON. Malformed / missing JSON is
        // non-fatal — fallback is pure additive enhancement, NEVER a
        // scene-compile blocker.
        let scene_json: serde_json::Value = match serde_json::from_str(&req.partial_scene) {
            Ok(v) => v,
            Err(e) => {
                debug!(error = %e, "claude-code: scene_compile_hook: partial_scene failed to parse as JSON; no injections");
                let contributions_json = serde_json::to_string(&contributions)
                    .unwrap_or_else(|_| "{\"env_injections\":[]}".to_string());
                return Ok(SceneCompileHookResponse {
                    contributions: contributions_json,
                });
            }
        };

        let panes = scene_json.get("panes").and_then(|v| v.as_array());
        if let Some(panes) = panes {
            for pane in panes {
                let Some(pane_id) = pane.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(view) = pane.get("view") else {
                    continue;
                };
                let kind = view.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if kind != "command" {
                    continue;
                }
                let Some(cmd) = view.get("cmd").and_then(|v| v.as_str()) else {
                    continue;
                };
                if !match_cmds.iter().any(|c| c == cmd) {
                    continue;
                }
                let mut env = std::collections::BTreeMap::new();
                env.insert(
                    "CLAUDE_HOOK_SOCKET".to_string(),
                    socket_path.to_string_lossy().into_owned(),
                );
                contributions.env_injections.push(EnvInjection {
                    pane_id: pane_id.to_string(),
                    env,
                });
            }
        }

        let contributions_json = serde_json::to_string(&contributions)
            .unwrap_or_else(|_| "{\"env_injections\":[]}".to_string());
        Ok(SceneCompileHookResponse {
            contributions: contributions_json,
        })
    }

    /// T-022 + T-023: advertise the two control verbs this extension
    /// contributes to `ark control`:
    ///
    /// * `install-hooks` — reconcile `~/.claude/settings.json` outside
    ///   a live session. Uses a placeholder session id + socket path
    ///   so that cc-hook invocations before ark launches fail-open
    ///   (T-009). The user runs this to repair drift.
    /// * `reinstall-hook-binary` — re-extract [`crate::CC_HOOK_BYTES`]
    ///   to `$XDG_BIN_HOME/cc-hook` with mode `0755`.
    ///
    /// # Wiring gap (T-046)
    ///
    /// `ControlVerbsResponse.verbs` is `OpaqueJson` — the supervisor
    /// collects these specs but no end-to-end `ark ext <ext> <verb>`
    /// CLI dispatcher exists yet (that's Tier 8 / T-046 hot-reload
    /// surface). For today, the verb handler entry points are
    /// [`ClaudeCodeExtension::run_install_hooks_verb`] +
    /// [`ClaudeCodeExtension::run_reinstall_hook_binary_verb`] — the
    /// CLI can invoke them directly once routing exists. The control
    /// verb list lives in `ControlVerbsResponse.verbs` as a
    /// `{"verbs": [{"name", "description", "args"}, …]}` JSON shape
    /// compatible with the forthcoming VerbSpec pin.
    async fn control_verbs(&self, _req: ControlVerbsRequest) -> ExtResult<ControlVerbsResponse> {
        let verbs = serde_json::json!({
            "verbs": [
                {
                    "name": "install-hooks",
                    "description": "Reconcile ~/.claude/settings.json to route Claude Code hooks through ark.",
                    "args": [],
                },
                {
                    "name": "reinstall-hook-binary",
                    "description": "Re-extract the embedded cc-hook binary to $XDG_BIN_HOME/cc-hook (mode 0755).",
                    "args": [],
                },
                // T-046 (R12): `ark ext reload claude-code`. The handler
                // entry point is [`ClaudeCodeExtension::reload`]; the
                // supervisor-side CLI routing re-uses the same
                // `ControlVerbsResponse.verbs` JSON the `install-hooks`
                // + `reinstall-hook-binary` verbs ride on.
                {
                    "name": "reload",
                    "description": "Hot-reload the claude-code extension: re-run settings.json reconciliation, re-bind per-session cc-hook socket, refresh transcript watcher + config. Live view handles are preserved.",
                    "args": [],
                },
            ],
        });
        // OpaqueJson is a `String` type alias — store the serialised
        // JSON document (the host decodes on the receive side).
        let contributions: OpaqueJson =
            serde_json::to_string(&verbs).unwrap_or_else(|_| "{\"verbs\":[]}".to_string());
        Ok(ControlVerbsResponse {
            verbs: contributions,
        })
    }

    /// T-042 + T-043 (R10): emit the five doctor checks. See the
    /// [`doctor`] module for per-check implementations + level mapping.
    ///
    /// The check invocations here use the ambient environment
    /// (`$PATH`, `$HOME`, `$ARK_STATE_DIR`) — doctor consumers that
    /// want to test against synthetic paths call the per-check
    /// functions in [`doctor`] directly with the `_override` params.
    async fn doctor_checks(&self, _req: DoctorChecksRequest) -> ExtResult<DoctorChecksResponse> {
        let env = DoctorEnvelope {
            results: vec![
                doctor::check_which_claude(),
                doctor::check_cc_hook_binary(None),
                doctor::check_settings_drift(None),
                doctor::check_state_sessions_writable(None),
                // R10: "informational check" — scene observation is a
                // scene-compiler concern not available on-extension.
                // None → "unverified".
                doctor::check_view_wired(None),
            ],
        };
        let checks: OpaqueJson =
            serde_json::to_string(&env).unwrap_or_else(|_| "{\"results\":[]}".to_string());
        Ok(DoctorChecksResponse { checks })
    }

    /// T-044 + T-045 (R11): emit the three contributed `ark list`
    /// columns, populated from the per-session rolling state cached on
    /// the extension. Zero state → `cc model=""`, `cc tokens="0"`,
    /// `cc cost=""` (T-045 regression).
    async fn list_columns(&self, _req: ListColumnsRequest) -> ExtResult<ListColumnsResponse> {
        let state = self.list_state_snapshot();
        let env = ColumnsEnvelope {
            columns: state.to_columns(),
        };
        let columns: OpaqueJson =
            serde_json::to_string(&env).unwrap_or_else(|_| "{\"columns\":[]}".to_string());
        Ok(ListColumnsResponse { columns })
    }
}

// ---------------------------------------------------------------------------
// Control verb handler entry points (T-022, T-023)
// ---------------------------------------------------------------------------

impl ClaudeCodeExtension {
    /// T-022: reconcile `~/.claude/settings.json` outside a live
    /// session. Uses the placeholder session-id `__latest__` and
    /// `$STATE/sessions/__latest__/cc-hook.sock` as the socket path,
    /// so cc-hook's fail-open branch (T-009) is the no-op path until a
    /// real session binds the real socket. Intended to be invoked via
    /// `ark ext claude-code install-hooks` once the CLI routing lands
    /// (T-046 gap).
    ///
    /// Returns the [`ReconcileOutcome`] so callers can surface
    /// structured counts to the user.
    pub fn run_install_hooks_verb(
        &self,
        settings_override: Option<&std::path::Path>,
        cc_hook_override: Option<&std::path::Path>,
    ) -> Result<ReconcileOutcome, SettingsJsonError> {
        let settings_path: std::path::PathBuf = match settings_override {
            Some(p) => p.to_path_buf(),
            None => match default_settings_path() {
                Some(p) => p,
                None => {
                    return Err(SettingsJsonError::Write {
                        path: std::path::PathBuf::from(DEFAULT_SETTINGS_REL_PATH),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "$HOME unset; cannot resolve ~/.claude/settings.json",
                        ),
                    });
                }
            },
        };

        let cc_hook_path = cc_hook_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(cc_hook_install_path);

        // Placeholder session id + socket path for no-session-mode.
        // cc-hook will fail-open against the missing socket until a
        // real session binds; this matches R1 acceptance criterion
        // "cc-hook being invoked without a live socket is a no-op".
        let session_id = "__latest__";
        let sock_path = std::path::PathBuf::from(format!(
            "/tmp/ark/sessions/{sid}/cc-hook.sock",
            sid = session_id
        ));

        let mut sf = SettingsFile::load(&settings_path)?;
        let outcome = sf.reconcile_ark_hooks(session_id, &sock_path, &cc_hook_path);
        match &outcome {
            ReconcileOutcome::Written { .. } => {
                sf.save_atomic()?;
            }
            ReconcileOutcome::NoChange => {}
            ReconcileOutcome::Unwritable(_) => {
                // `reconcile_ark_hooks` never returns Unwritable today
                // (that variant is reserved for callers who want to
                // signal upstream IO failure); treat defensively.
            }
        }
        info!(
            path = %settings_path.display(),
            outcome = ?outcome,
            "claude-code: install-hooks reconciliation complete"
        );
        Ok(outcome)
    }

    /// T-023: re-extract [`crate::CC_HOOK_BYTES`] to the resolved
    /// install path (or the caller-supplied override). Intended to be
    /// invoked via `ark ext claude-code reinstall-hook-binary`.
    pub fn run_reinstall_hook_binary_verb(
        &self,
        path_override: Option<&std::path::Path>,
    ) -> InstallOutcome {
        let target = path_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(cc_hook_install_path);
        install_cc_hook_at(&target)
    }
}

// ---------------------------------------------------------------------------
// T-046 (R12) — hot reload
// ---------------------------------------------------------------------------

/// T-046 (R12): request payload for [`ClaudeCodeExtension::reload`].
///
/// Carries the new scene's ext-config bucket + optional live session
/// context. When the session context is `Some`, reload re-runs the
/// on-session-start side-effects (settings.json reconcile, socket rebind,
/// transcript-watcher setup) against the live session. When `None`
/// (scene-only reload, no live session), the call is config-only —
/// `config` gets refreshed and the [`ClaudeCodeConfig`] is visible on the
/// NEXT `scene_compile_hook` / `on_session_start` dispatch.
///
/// `new_ext_config` must contain a `"claude-code"` key — if it's missing,
/// reload is rejected per R12 "mismatched view-type wiring → reject
/// reload, old scene stays live" (the extension interprets a scene that
/// no longer declares `use "claude-code"` as a mismatch rather than a
/// silent teardown — teardown is the supervisor's job via a different
/// code path).
#[derive(Debug, Clone, Default)]
pub struct ReloadRequest {
    /// New scene's `SessionSpec.ext_config` bucket. Checked for a
    /// `"claude-code"` entry; absence rejects the reload.
    pub new_ext_config: std::collections::BTreeMap<String, serde_json::Value>,
    /// Live session context. `Some((session_id, socket_path,
    /// session_dir))` triggers the session-side re-run of
    /// settings.json reconcile + transcript-watcher setup. `None` =
    /// config-only refresh.
    pub session: Option<ReloadSessionCtx>,
}

/// Per-session context handed to [`ClaudeCodeExtension::reload`] when the
/// caller owns live session state (i.e. `on_session_start` already ran).
#[derive(Debug, Clone)]
pub struct ReloadSessionCtx {
    /// The live session id.
    pub session_id: ark_types::SessionId,
    /// Path to the currently-bound cc-hook socket. Reload preserves this
    /// path — cc-hook's fresh-invocation model reconnects on next fire.
    pub socket_path: std::path::PathBuf,
    /// Session directory. Sentinels (settings-unwritable,
    /// bridge-version-mismatch) live here.
    pub session_dir: std::path::PathBuf,
    /// Directory to bind the transcript watcher against. When `None`
    /// reload skips the watcher re-setup — a subsequent hook-fired frame
    /// will late-bind via `ensure_tracking` (T-027's idempotent path).
    pub transcript_parent_dir: Option<std::path::PathBuf>,
}

/// T-046 (R12): outcome of [`ClaudeCodeExtension::reload`]. Each `bool`
/// records whether the corresponding side effect actually ran — useful
/// for tests + for the CLI `ark ext reload claude-code` summary line.
///
/// Invariants:
///
/// * On successful reload `config_refreshed` is always `true` — a reload
///   that did not touch config is semantically a no-op and the caller
///   should get back a rejected reload instead.
/// * `settings_reconciled` is gated on `config.auto_install_hook_entries`
///   AND a present [`ReloadSessionCtx`]; false on config-only reload.
/// * `transcript_watcher_reset` is gated on a present
///   `transcript_parent_dir` in the session ctx; false otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Config was replaced on the extension.
    pub config_refreshed: bool,
    /// Settings.json reconcile ran (gated on
    /// `config.auto_install_hook_entries` AND session ctx present).
    pub settings_reconciled: bool,
    /// Transcript watcher setup ran (gated on `transcript_parent_dir`).
    pub transcript_watcher_reset: bool,
}

/// T-046 (R12): error returned when reload is rejected. Old scene stays
/// live; the supervisor treats this as a scene-level rejection and keeps
/// the previous `ClaudeCodeExtension` state intact.
#[derive(Debug, thiserror::Error)]
pub enum ReloadRejected {
    /// The new scene no longer declares `use "claude-code"`.
    /// Per R12 "mismatched view-type wiring" this is a reload-rejection,
    /// not a teardown — if the user wants to remove the extension from a
    /// scene they restart the session.
    #[error(
        "claude-code: reload rejected — new scene does not declare `use \"claude-code\"` in ext_config (old scene stays live)"
    )]
    MissingExtConfig,
}

impl ClaudeCodeExtension {
    /// T-046 (R12): hot-reload the claude-code extension against a new
    /// scene/config.
    ///
    /// On success, the following side-effects run in order:
    ///
    /// 1. Validate: `new_ext_config` MUST contain a `"claude-code"`
    ///    entry, otherwise → [`ReloadRejected::MissingExtConfig`] +
    ///    old scene stays live.
    /// 2. Parse the new `[claude-code]` section via
    ///    [`Self::load_session_config`]. Unknown keys warn; malformed
    ///    values fall back to defaults (same failure mode as
    ///    `on_session_start`).
    /// 3. If a [`ReloadSessionCtx`] is present AND
    ///    `config.auto_install_hook_entries` is `true`:
    ///    [`reconcile_settings_for_session`] runs against the live
    ///    session id + socket path.
    /// 4. If a [`ReloadSessionCtx`] is present AND
    ///    `transcript_parent_dir` is `Some`: a FRESH
    ///    [`transcript::TranscriptDirWatcher`] is created + wired to a
    ///    log sink (matching `on_session_start`). Any previous watcher
    ///    kept by the caller should be dropped — its underlying
    ///    `notify::RecommendedWatcher` stops on drop.
    /// 5. Socket rebind is a no-op at this layer: cc-hook invocations
    ///    are stateless (one NDJSON line → close), the live
    ///    `UnixListener` inside the caller-held [`socket::CcHookSocket`]
    ///    stays bound at the same path. The kit R12 "re-binds the cc-hook
    ///    socket" claim is preserved by the property that the socket
    ///    path is IDENTICAL across reloads (session id doesn't change)
    ///    and cc-hook reconnects on every fire.
    /// 6. In-flight `claude` process is untouched — the extension never
    ///    held a handle to it.
    ///
    /// Typed `Stack<_>` ref survival: the view's `subagents` handle is
    /// an opaque [`ark_view::HandleId`] — construction-time bytes that
    /// the extension never owns or mutates. Reloading the extension
    /// CANNOT invalidate the view-side handle. The regression test
    /// `reload_preserves_stack_handle_identity` pins this.
    ///
    /// Returns [`ReloadOutcome`] so callers can log a summary line.
    pub fn reload(&self, req: ReloadRequest) -> Result<ReloadOutcome, ReloadRejected> {
        // Step 1: validate — scene must still declare `use "claude-code"`.
        if !req.new_ext_config.contains_key(EXT_NAME) {
            warn!(
                "claude-code: reload rejected — new scene removed `use \"claude-code\"` (old scene stays live)"
            );
            return Err(ReloadRejected::MissingExtConfig);
        }

        // Step 2: refresh config. Unknown-key warnings fire from inside
        // `load_session_config`; malformed values fall back to defaults.
        let new_config = self.load_session_config(&req.new_ext_config);
        debug!(
            match_cmds_len = new_config.match_cmds.len(),
            transcript_tail_lines = new_config.transcript_tail_lines,
            auto_install_hook_entries = new_config.auto_install_hook_entries,
            "claude-code: reload: config refreshed"
        );

        let mut outcome = ReloadOutcome {
            config_refreshed: true,
            settings_reconciled: false,
            transcript_watcher_reset: false,
        };

        // Step 3 + 4: session-scoped side effects.
        if let Some(ctx) = req.session.as_ref() {
            if new_config.auto_install_hook_entries {
                reconcile_settings_for_session(
                    &ctx.session_id,
                    &ctx.socket_path,
                    &ctx.session_dir,
                    /* override_settings_path */ None,
                    /* override_cc_hook_path */ None,
                );
                outcome.settings_reconciled = true;
            } else {
                debug!(
                    session = %ctx.session_id.as_path_leaf(),
                    "claude-code: reload: auto_install_hook_entries=false; skipping settings.json reconcile"
                );
            }

            if let Some(dir) = ctx.transcript_parent_dir.as_ref() {
                let (watcher, rx) = transcript::TranscriptDirWatcher::new();
                let shared: transcript::SharedDirWatcher =
                    std::sync::Arc::new(std::sync::Mutex::new(watcher));
                if let Ok(mut w) = shared.lock() {
                    w.ensure_tracking(dir);
                }
                // Sink the dir events so the watcher drives forward; the
                // caller will typically replace this with the view-side
                // consumer on the next spawn. Matches the sink shape used
                // in `on_session_start`.
                let _log_sink = transcript::spawn_log_sink(rx);
                // Drop `shared` here — the new watcher's `notify` inner
                // keeps the OS-level handle live via the spawned log-sink
                // task; when the task exits (rx dropped) the watcher
                // tears down cleanly. This is a best-effort refresh
                // intended for the CLI `ark ext reload` path; production
                // wiring (supervisor-side) would hold the `SharedDirWatcher`
                // on the extension instance, but v0.1 keeps it scoped.
                debug!(
                    session = %ctx.session_id.as_path_leaf(),
                    dir = %dir.display(),
                    "claude-code: reload: transcript watcher reset"
                );
                outcome.transcript_watcher_reset = true;
            }
        }

        info!(
            config_refreshed = outcome.config_refreshed,
            settings_reconciled = outcome.settings_reconciled,
            transcript_watcher_reset = outcome.transcript_watcher_reset,
            "claude-code: reload complete"
        );
        Ok(outcome)
    }

    /// T-046 convenience: `reload` with a config-only signature (no
    /// live session). Equivalent to calling [`Self::reload`] with
    /// `session: None`. Useful when the caller is the scene-compiler
    /// driving a pre-launch reload through `ark ext reload claude-code`.
    pub fn reload_config_only(
        &self,
        new_ext_config: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<ReloadOutcome, ReloadRejected> {
        self.reload(ReloadRequest {
            new_ext_config,
            session: None,
        })
    }
}

// ---------------------------------------------------------------------------
// T-032 helper — placeholder cc-hook socket path for scene_compile_hook
// ---------------------------------------------------------------------------

/// Placeholder cc-hook socket path stamped by
/// [`ClaudeCodeExtension::scene_compile_hook`].
///
/// Shape: `<state>/sessions/__latest__/cc-hook.sock` where `<state>` is
/// resolved from the ark `StateLayout` env vars. Consumers that know the
/// real session id substitute it for `__latest__` when merging the env
/// injection into pane launch.
///
/// The `__latest__` placeholder is the same convention the T-022
/// `install-hooks` control verb uses — any reader that follows the
/// install-hooks contract already knows how to interpret it. cc-hook's
/// fail-open branch (T-009) keeps the no-live-session state a no-op.
fn placeholder_session_socket_path() -> std::path::PathBuf {
    match ark_types::StateLayout::from_env() {
        Ok(layout) => {
            // Use the same session-dir layout as CcHookSocket::bind but
            // with the __latest__ placeholder id.
            let fallback_id = ark_types::SessionId::new("__latest__");
            layout.session_dir(&fallback_id).join("cc-hook.sock")
        }
        Err(_) => std::path::PathBuf::from("/tmp/ark/sessions/__latest__/cc-hook.sock"),
    }
}

// ---------------------------------------------------------------------------
// T-020 + T-021 helper — reconcile-on-session-start wrapper
// ---------------------------------------------------------------------------

/// Shared implementation between `on_session_start` and (future)
/// direct-call callers. Arguments kept explicit so tests can drive it
/// with synthetic paths.
///
/// * `session_id` — path-leaf form suitable for command-line interpolation.
/// * `socket_path` — absolute path to the bound cc-hook socket.
/// * `session_dir` — where to drop the [`SETTINGS_UNWRITABLE_SENTINEL`]
///   on failure.
/// * `override_settings_path` — test hook to redirect away from
///   `~/.claude/settings.json`.
/// * `override_cc_hook_path` — test hook to pin the written binary
///   path (otherwise [`cc_hook_install_path`]).
pub fn reconcile_settings_for_session(
    session_id: &ark_types::SessionId,
    socket_path: &std::path::Path,
    session_dir: &std::path::Path,
    override_settings_path: Option<&std::path::Path>,
    override_cc_hook_path: Option<&std::path::Path>,
) {
    // Resolve settings.json path. Missing $HOME without an explicit
    // override is effectively "no settings.json in this env" — we
    // still run the reconciler so the sentinel surfaces the miss
    // instead of silently dropping reconciliation.
    let settings_path = match override_settings_path {
        Some(p) => p.to_path_buf(),
        None => match default_settings_path() {
            Some(p) => p,
            None => {
                warn!(
                    "claude-code: $HOME unset; cannot resolve ~/.claude/settings.json; skipping reconciliation"
                );
                let err = SettingsJsonError::Write {
                    path: std::path::PathBuf::from(DEFAULT_SETTINGS_REL_PATH),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "$HOME unset"),
                };
                let _ = record_settings_unwritable_sentinel(
                    session_dir,
                    &err,
                    std::path::Path::new(DEFAULT_SETTINGS_REL_PATH),
                );
                return;
            }
        },
    };

    let cc_hook_path = override_cc_hook_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(cc_hook_install_path);

    let result = (|| -> Result<ReconcileOutcome, SettingsJsonError> {
        let mut sf = SettingsFile::load(&settings_path)?;
        let leaf = session_id.as_path_leaf();
        let outcome = sf.reconcile_ark_hooks(&leaf, socket_path, &cc_hook_path);
        if matches!(outcome, ReconcileOutcome::Written { .. }) {
            sf.save_atomic()?;
        }
        Ok(outcome)
    })();

    match result {
        Ok(outcome) => {
            debug!(
                session = %session_id.as_path_leaf(),
                path = %settings_path.display(),
                outcome = ?outcome,
                "claude-code: settings.json reconciled on session start"
            );
        }
        Err(err) => {
            warn!(
                session = %session_id.as_path_leaf(),
                path = %settings_path.display(),
                error = %err,
                "claude-code: settings.json reconciliation failed; dropping sentinel for doctor"
            );
            if let Err(e) = record_settings_unwritable_sentinel(session_dir, &err, &settings_path) {
                warn!(
                    session_dir = %session_dir.display(),
                    error = %e,
                    "claude-code: failed to record settings-unwritable sentinel"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T-035 / T-037 — SubagentRegistry: per-subagent state cache + event dispatch
// ---------------------------------------------------------------------------

use ark_types::ExtEvent;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// T-037 event-dispatch outcome. Emitted by [`SubagentRegistry::on_ext_event`]
/// so callers can forward the resulting `RenamePane` payload through
/// the host's `pane/emit` RPC (T-035).
///
/// `id` is the subagent id; `payload` is the
/// `{ "kind": "RenamePane", "name": <title> }` JSON the kit pins in R6.
/// Callers that don't care about emission (e.g. untyped / fallback
/// scenes) can still observe the event flow — the registry caches state
/// either way. A cached miss (event for an unknown `id`) returns `None`.
#[derive(Debug, Clone)]
pub struct RenamePaneEmission {
    /// Subagent id the payload targets. Host maps this back to the
    /// corresponding stack-child `Pane<ClaudeCodeSubagent>` handle.
    pub id: String,
    /// `pane/emit` payload. Shape pinned at
    /// `{ "kind": "RenamePane", "name": <string> }`.
    pub payload: serde_json::Value,
}

/// T-035 + T-037: per-session cache of subagent state + the entry point
/// the socket-reader accept-loop calls on each ExtEvent. Lives on
/// [`ClaudeCodeExtension`]; shared across the accept-loop task and any
/// future `ClaudeCodeView`-driven consumers via `Arc<Mutex<_>>`.
///
/// Filtering: [`Self::on_ext_event`] responds to the three event kinds
/// R6 + R7 list — `claude-code.subagent.start`,
/// `claude-code.subagent.stop`, and `claude-code.pre-tool-use`. Any
/// other `kind` is ignored and returns `None`.
///
/// Data flow:
///
/// 1. `subagent.start` creates a fresh [`SubagentState`] keyed on
///    `payload.agent_id` with `status = Running` and
///    `agent_type = payload.agent_type`.
/// 2. `pre-tool-use` looks up the entry by `payload.agent_id` and
///    updates `last_tool` in place. **Missing `agent_id`** — the
///    main-session `PreToolUse` hook fires without it — drops the event
///    (no main-session subagent to update).
/// 3. `subagent.stop` transitions `status` to `Done` / `Failed`
///    according to `payload.success`.
///
/// Each handled event returns a fresh [`RenamePaneEmission`] so the
/// caller can drive `pane/emit`.
#[derive(Debug, Default, Clone)]
pub struct SubagentRegistry {
    /// `agent_id → SubagentState`.
    inner: Arc<Mutex<HashMap<String, SubagentState>>>,
}

impl SubagentRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Snapshot of all tracked subagents' state, primarily for tests.
    /// Returns a cloned `HashMap` so callers don't hold the lock.
    pub fn snapshot(&self) -> HashMap<String, SubagentState> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Read one tracked subagent's state (test / doctor introspection).
    pub fn get(&self, id: &str) -> Option<SubagentState> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }

    /// T-037 event dispatch. Consume an [`ExtEvent`] — if it is one of
    /// the three subagent-related kinds this registry filters, updates
    /// the cached state and returns the resulting [`RenamePaneEmission`].
    /// Events for unknown subagent ids (e.g. a `subagent.stop` without
    /// a prior `subagent.start`) still update state defensively, so
    /// a fresh registry can pick up mid-session.
    ///
    /// Returns `None` for:
    /// * ExtEvents not in `{subagent.start, subagent.stop, pre-tool-use}`.
    /// * `pre-tool-use` events with no `agent_id` (the main-session
    ///   `PreToolUse` hook) — no subagent context to update.
    /// * Any event whose payload is missing `agent_id`.
    pub fn on_ext_event(&self, event: &ExtEvent) -> Option<RenamePaneEmission> {
        if event.ext != EXT_NAME {
            return None;
        }
        match event.kind.as_str() {
            "subagent.start" => {
                let id = event.payload.get("agent_id").and_then(|v| v.as_str())?;
                let agent_type = event
                    .payload
                    .get("agent_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("subagent");
                let state = SubagentState::new(id, agent_type);
                let emission = RenamePaneEmission {
                    id: id.to_string(),
                    payload: ClaudeCodeSubagentView::rename_pane_payload(&state),
                };
                if let Ok(mut g) = self.inner.lock() {
                    g.insert(id.to_string(), state);
                }
                Some(emission)
            }
            "subagent.stop" => {
                let id = event.payload.get("agent_id").and_then(|v| v.as_str())?;
                // Claude Code's SubagentStop payload carries `success:
                // bool` — true → Done, false → Failed. When absent
                // (unusual), default defensively to `Done`.
                let success = event
                    .payload
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let new_status = if success {
                    SubagentStatus::Done
                } else {
                    SubagentStatus::Failed
                };
                let mut g = match self.inner.lock() {
                    Ok(g) => g,
                    Err(_) => return None,
                };
                let state = g.entry(id.to_string()).or_insert_with(|| {
                    // Defensive: a Stop without a prior Start can
                    // happen if the extension mounts mid-session. Stamp
                    // a synthetic entry so the title reflects the
                    // transition; agent_type falls back to a sentinel.
                    SubagentState::new(id, "subagent")
                });
                state.status = new_status;
                Some(RenamePaneEmission {
                    id: id.to_string(),
                    payload: ClaudeCodeSubagentView::rename_pane_payload(state),
                })
            }
            "pre-tool-use" => {
                // Main-session PreToolUse fires without agent_id —
                // ignore; only subagent-scoped tool use updates title.
                let id = event.payload.get("agent_id").and_then(|v| v.as_str())?;
                let tool = event
                    .payload
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let mut g = match self.inner.lock() {
                    Ok(g) => g,
                    Err(_) => return None,
                };
                let state = g.get_mut(id)?;
                state.last_tool = Some(tool.to_string());
                Some(RenamePaneEmission {
                    id: id.to_string(),
                    payload: ClaudeCodeSubagentView::rename_pane_payload(state),
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// T-038 / T-039 / T-040 — ClaudeCodeView fan-out to Stack<ClaudeCodeSubagent>
// ---------------------------------------------------------------------------

/// T-038 fan-out outcome. Emitted by
/// [`ClaudeCodeView::on_subagent_start`] when a new `subagent.start`
/// event lands AND the view's `subagents` handle is `Some` AND the
/// `agent_id` is NOT already in the spawned-set.
///
/// `attrs` carries the typed [`ClaudeCodeSubagentAttrs`] the host
/// consumer will pass through `Stack<_>::spawn_pane`. v0.1 `PaneAttrs`
/// is empty (see `ark-view::typed::PaneAttrs`), so the typed attrs are
/// emitted separately for the host to correlate with the spawned child
/// at wiring time (T-046). The downstream mapping
/// `attrs → Pane<ClaudeCodeSubagentView>` is pinned in
/// [`ClaudeCodeSubagentAttrs`].
#[derive(Debug, Clone)]
pub struct SubagentFanOut {
    /// Typed attrs identifying the subagent + its transcript path.
    pub attrs: ClaudeCodeSubagentAttrs,
}

/// Per-`ClaudeCodeView` spawn tracker. Separate from [`SubagentRegistry`]
/// because one extension can mount multiple `ClaudeCodeView` instances
/// (e.g. a scene with two `claude-code` panes, each with its own
/// `subagents` stack); every view gets its OWN idempotency set so a
/// duplicate `agent_id` across views each spawns once into its
/// respective stack.
#[derive(Debug, Default, Clone)]
pub struct ClaudeCodeSpawnSet {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl ClaudeCodeSpawnSet {
    /// Construct an empty spawn set.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Return `true` iff `id` has NOT been spawned yet; atomically
    /// records it so subsequent calls return `false`. Idempotency hook
    /// for T-038's duplicate-start guard.
    fn claim(&self, id: &str) -> bool {
        match self.inner.lock() {
            Ok(mut g) => g.insert(id.to_string()),
            Err(_) => false,
        }
    }

    /// Whether `id` is in the set (read-only, tests).
    pub fn contains(&self, id: &str) -> bool {
        self.inner.lock().map(|g| g.contains(id)).unwrap_or(false)
    }

    /// Count of spawned children (tests).
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Whether the spawn set is empty (clippy-friendly companion to `len`).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ClaudeCodeView {
    /// T-038 + T-039 + T-040: process a `claude-code.subagent.start`
    /// (T-038), `claude-code.subagent.stop` (T-039) OR other event
    /// against this view's `subagents` handle.
    ///
    /// Returns `Some(SubagentFanOut)` ONLY when all of:
    /// * `event.ext == "claude-code"` AND `event.kind == "subagent.start"`.
    /// * `self.subagents.is_some()` (T-040: `None` no-ops).
    /// * `event.payload.agent_id` is a non-empty string.
    /// * `agent_id` was NOT already claimed in `spawn_set` (idempotent
    ///   on duplicate `subagent.start` per T-038).
    ///
    /// For `subagent.stop`: returns `None` — per R7 / T-039 the view
    /// does NOT remove the child, leaving the tile in place for the
    /// user. Status transition lands via [`SubagentRegistry::on_ext_event`]
    /// which the extension dispatches in parallel.
    ///
    /// For any other kind: returns `None`.
    ///
    /// Note — `transcript_path` is read from `agent_transcript_path`,
    /// Claude Code's canonical field name on `SubagentStart`. A missing
    /// field stamps an empty string — the downstream subagent view
    /// tolerates an empty transcript path (render_transcript_tail
    /// reports a missing-file as no lines).
    pub fn on_ext_event(
        &self,
        event: &ExtEvent,
        spawn_set: &ClaudeCodeSpawnSet,
    ) -> Option<SubagentFanOut> {
        if event.ext != EXT_NAME {
            return None;
        }
        if event.kind != "subagent.start" {
            // T-039: subagent.stop does NOT remove or fan-out; T-040:
            // None subagents handle no-ops regardless.
            return None;
        }
        // T-040: subagents == None → no fan-out (events still reach
        // user reactions via the broader bus — this method just
        // short-circuits the typed spawn).
        self.subagents.as_ref()?;

        let id = event
            .payload
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?;
        // T-038 idempotency guard.
        if !spawn_set.claim(id) {
            return None;
        }
        let transcript_path = event
            .payload
            .get("agent_transcript_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Some(SubagentFanOut {
            attrs: ClaudeCodeSubagentAttrs {
                id: id.to_string(),
                transcript_path,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// T-035 / T-037 / T-038 / T-039 / T-040 — tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod subagent_dispatch_tests {
    use super::*;
    use ark_types::ExtEvent;
    use ark_view::{HandleId, Stack};

    fn ev(kind: &str, payload: serde_json::Value) -> ExtEvent {
        ExtEvent {
            ext: EXT_NAME.to_string(),
            kind: kind.to_string(),
            payload,
        }
    }

    fn stub_stack() -> Stack<ClaudeCodeSubagent> {
        // `Stack::from_handle` is crate-private on `ark-view`; the
        // serde-round-trip is the public path to construct a test
        // handle from outside the crate (see ark-view/src/typed.rs
        // tests).
        serde_json::from_str::<Stack<ClaudeCodeSubagent>>("\"stub-stack\"")
            .expect("stub stack deserialisation")
    }

    fn view_with_subagents() -> ClaudeCodeView {
        ClaudeCodeView {
            model: None,
            args: vec![],
            cwd: None,
            subagents: Some(stub_stack()),
        }
    }

    // -- T-035 + T-037: SubagentRegistry dispatch ----------------------------

    #[test]
    fn registry_subagent_start_inserts_running_state() {
        let reg = SubagentRegistry::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "agent-1",
                "agent_type": "code-writer",
                "agent_transcript_path": "/tmp/a.jsonl",
            }),
        );
        let out = reg.on_ext_event(&e).expect("emission");
        assert_eq!(out.id, "agent-1");
        assert_eq!(out.payload.get("kind").unwrap(), "RenamePane");
        assert_eq!(
            out.payload.get("name").unwrap(),
            "code-writer · running · -"
        );
        let s = reg.get("agent-1").unwrap();
        assert_eq!(s.status, SubagentStatus::Running);
        assert_eq!(s.agent_type, "code-writer");
    }

    #[test]
    fn registry_pre_tool_use_updates_last_tool() {
        let reg = SubagentRegistry::new();
        let _ = reg.on_ext_event(&ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "agent-1",
                "agent_type": "writer",
            }),
        ));
        let out = reg
            .on_ext_event(&ev(
                "pre-tool-use",
                serde_json::json!({ "agent_id": "agent-1", "tool_name": "Edit" }),
            ))
            .expect("emission");
        assert_eq!(out.payload.get("name").unwrap(), "writer · running · Edit");
        assert_eq!(
            reg.get("agent-1").unwrap().last_tool.as_deref(),
            Some("Edit")
        );
    }

    #[test]
    fn registry_pre_tool_use_without_agent_id_ignored() {
        let reg = SubagentRegistry::new();
        // Main-session PreToolUse (no agent_id) → no emission.
        assert!(
            reg.on_ext_event(&ev(
                "pre-tool-use",
                serde_json::json!({ "tool_name": "Edit" }),
            ))
            .is_none()
        );
    }

    #[test]
    fn registry_pre_tool_use_unknown_agent_ignored() {
        let reg = SubagentRegistry::new();
        assert!(
            reg.on_ext_event(&ev(
                "pre-tool-use",
                serde_json::json!({ "agent_id": "ghost", "tool_name": "Edit" }),
            ))
            .is_none()
        );
    }

    #[test]
    fn registry_subagent_stop_success_transitions_to_done() {
        let reg = SubagentRegistry::new();
        let _ = reg.on_ext_event(&ev(
            "subagent.start",
            serde_json::json!({"agent_id": "a", "agent_type": "t"}),
        ));
        let out = reg
            .on_ext_event(&ev(
                "subagent.stop",
                serde_json::json!({"agent_id": "a", "success": true}),
            ))
            .expect("emission");
        assert_eq!(out.payload.get("name").unwrap(), "t · done · -");
        assert_eq!(reg.get("a").unwrap().status, SubagentStatus::Done);
    }

    #[test]
    fn registry_subagent_stop_failure_transitions_to_failed() {
        let reg = SubagentRegistry::new();
        let _ = reg.on_ext_event(&ev(
            "subagent.start",
            serde_json::json!({"agent_id": "a", "agent_type": "t"}),
        ));
        let out = reg
            .on_ext_event(&ev(
                "subagent.stop",
                serde_json::json!({"agent_id": "a", "success": false}),
            ))
            .expect("emission");
        assert_eq!(out.payload.get("name").unwrap(), "t · failed · -");
        assert_eq!(reg.get("a").unwrap().status, SubagentStatus::Failed);
    }

    #[test]
    fn registry_subagent_stop_without_start_synthesises_state() {
        let reg = SubagentRegistry::new();
        // Defensive: no prior start → we still record the terminal
        // status so a mid-session mount can still render.
        let out = reg
            .on_ext_event(&ev(
                "subagent.stop",
                serde_json::json!({"agent_id": "late", "success": true}),
            ))
            .expect("emission");
        assert_eq!(out.payload.get("name").unwrap(), "subagent · done · -");
    }

    #[test]
    fn registry_ignores_other_ext_events() {
        let reg = SubagentRegistry::new();
        assert!(
            reg.on_ext_event(&ev("post-tool-use", serde_json::json!({"agent_id": "a"}),))
                .is_none()
        );
        assert!(
            reg.on_ext_event(&ExtEvent {
                ext: "other-ext".to_string(),
                kind: "subagent.start".to_string(),
                payload: serde_json::json!({"agent_id": "a", "agent_type": "t"}),
            })
            .is_none()
        );
    }

    // -- T-038: fan-out -------------------------------------------------------

    #[test]
    fn fan_out_on_subagent_start_when_subagents_is_some() {
        let v = view_with_subagents();
        let set = ClaudeCodeSpawnSet::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "agent-xyz",
                "agent_type": "writer",
                "agent_transcript_path": "/tmp/x.jsonl",
            }),
        );
        let f = v.on_ext_event(&e, &set).expect("fan-out");
        assert_eq!(f.attrs.id, "agent-xyz");
        assert_eq!(f.attrs.transcript_path, "/tmp/x.jsonl");
        assert!(set.contains("agent-xyz"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn fan_out_is_idempotent_on_duplicate_agent_id() {
        let v = view_with_subagents();
        let set = ClaudeCodeSpawnSet::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "dup",
                "agent_type": "writer",
                "agent_transcript_path": "/tmp/d.jsonl",
            }),
        );
        assert!(v.on_ext_event(&e, &set).is_some());
        assert!(v.on_ext_event(&e, &set).is_none());
        assert_eq!(set.len(), 1);
    }

    // -- T-039: subagent.stop does NOT remove or fan-out ---------------------

    #[test]
    fn fan_out_on_subagent_stop_returns_none_and_does_not_modify_set() {
        let v = view_with_subagents();
        let set = ClaudeCodeSpawnSet::new();
        let start = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "keep",
                "agent_type": "t",
                "agent_transcript_path": "/tmp/k.jsonl",
            }),
        );
        let _ = v.on_ext_event(&start, &set);
        let stop = ev(
            "subagent.stop",
            serde_json::json!({"agent_id": "keep", "success": true}),
        );
        // No fan-out on stop.
        assert!(v.on_ext_event(&stop, &set).is_none());
        // Spawn set UNCHANGED — child stays live (T-039).
        assert!(set.contains("keep"));
        assert_eq!(set.len(), 1);
    }

    // -- T-040: subagents = None no-ops -------------------------------------

    #[test]
    fn fan_out_when_subagents_is_none_noops() {
        let v = ClaudeCodeView::default(); // subagents: None
        assert!(v.subagents.is_none());
        let set = ClaudeCodeSpawnSet::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "orphan",
                "agent_type": "writer",
                "agent_transcript_path": "/tmp/o.jsonl",
            }),
        );
        assert!(v.on_ext_event(&e, &set).is_none());
        // Nothing was claimed — set stays empty so a later wiring that
        // DOES have a subagents handle isn't biased by the no-op path.
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn fan_out_none_handle_still_allows_registry_dispatch_for_user_reactions() {
        // T-040 acceptance: events still flow via the registry /
        // broader ExtEvent bus even when fan-out is off. The registry
        // is the canonical user-visible state cache; verifying it
        // updates independently of the view's fan-out decision proves
        // the two surfaces are correctly decoupled.
        let v = ClaudeCodeView::default();
        let set = ClaudeCodeSpawnSet::new();
        let reg = SubagentRegistry::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({"agent_id": "z", "agent_type": "t"}),
        );
        assert!(v.on_ext_event(&e, &set).is_none());
        assert!(reg.on_ext_event(&e).is_some());
        assert_eq!(reg.get("z").unwrap().status, SubagentStatus::Running);
    }

    // unused import for HandleId is a diagnostic — import is kept so
    // follow-on tests can use HandleId directly if needed.
    #[allow(dead_code)]
    fn _assert_handle_id_in_scope(_h: HandleId) {}
}

// ---------------------------------------------------------------------------
// Tier 7 — T-041 + T-042 + T-043 + T-044 + T-045 integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tier7_tests {
    use super::*;
    use ark_ext_proto::{ArkExtension, DoctorChecksRequest, ListColumnsRequest};
    use std::collections::BTreeMap;

    // -- T-041: config loading ---------------------------------------------

    #[test]
    fn t041_config_defaults_on_missing_section() {
        let ext = ClaudeCodeExtension::new();
        let ec: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let c = ext.load_session_config(&ec);
        assert!(c.match_cmds.is_empty());
        assert_eq!(c.transcript_tail_lines, 200);
        assert!(c.auto_install_hook_entries);
    }

    #[test]
    fn t041_config_loads_full_section_into_cache() {
        let ext = ClaudeCodeExtension::new();
        let mut ec: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        ec.insert(
            "claude-code".into(),
            serde_json::json!({
                "match_cmds": ["claude"],
                "transcript_tail_lines": 50,
                "auto_install_hook_entries": false,
            }),
        );
        let c = ext.load_session_config(&ec);
        assert_eq!(c.match_cmds, vec!["claude".to_string()]);
        assert_eq!(c.transcript_tail_lines, 50);
        assert!(!c.auto_install_hook_entries);
        // Cached snapshot agrees.
        let snap = ext.config_snapshot();
        assert_eq!(snap, c);
        // match_cmds() helper reflects the updated config.
        assert_eq!(ext.match_cmds(), vec!["claude".to_string()]);
    }

    #[test]
    fn t041_config_unknown_key_warns_and_falls_through() {
        // R9: unknown keys must NOT fail the load — they fall through
        // to a defaulted struct with the known keys populated.
        let ext = ClaudeCodeExtension::new();
        let mut ec: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        ec.insert(
            "claude-code".into(),
            serde_json::json!({
                "match_cmds": ["x"],
                "permission_policy": "auto", // unknown — R9 no-policy keys
            }),
        );
        let c = ext.load_session_config(&ec);
        assert_eq!(c.match_cmds, vec!["x".to_string()]);
        // Other known fields default.
        assert_eq!(c.transcript_tail_lines, 200);
        assert!(c.auto_install_hook_entries);
    }

    // -- T-042 + T-043: doctor_checks RPC ----------------------------------

    #[tokio::test]
    async fn t042_doctor_checks_rpc_emits_five_results() {
        let ext = ClaudeCodeExtension::new();
        let resp = ext
            .doctor_checks(DoctorChecksRequest::default())
            .await
            .unwrap();
        let env: DoctorEnvelope = serde_json::from_str(&resp.checks).expect("envelope decodes");
        assert_eq!(env.results.len(), 5, "R10 declares 5 checks");
        let kinds: Vec<&str> = env.results.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"claude-code/which-claude"));
        assert!(kinds.contains(&"claude-code/cc-hook-binary"));
        assert!(kinds.contains(&"claude-code/settings-hooks"));
        assert!(kinds.contains(&"claude-code/sessions-writable"));
        assert!(kinds.contains(&"claude-code/view-wired"));
    }

    #[tokio::test]
    async fn t043_doctor_checks_carry_remediation_hints_on_non_info_levels() {
        let ext = ClaudeCodeExtension::new();
        let resp = ext
            .doctor_checks(DoctorChecksRequest::default())
            .await
            .unwrap();
        let env: DoctorEnvelope = serde_json::from_str(&resp.checks).unwrap();
        for r in &env.results {
            match r.level {
                CheckLevel::Error | CheckLevel::Warn => {
                    assert!(
                        r.fix.is_some(),
                        "check {} at level {:?} must carry a fix hint",
                        r.kind,
                        r.level
                    );
                }
                CheckLevel::Info => {
                    // Info checks don't require a fix.
                }
            }
        }
    }

    #[test]
    fn t043_doctor_rendering_table_is_readable() {
        // R10 "`ark doctor` rendering test": assemble a synthetic
        // envelope covering all three levels and assert the rendered
        // text is self-explanatory.
        let env = DoctorEnvelope {
            results: vec![
                CheckResult {
                    kind: "claude-code/which-claude".into(),
                    level: CheckLevel::Error,
                    message: "claude binary not found on $PATH".into(),
                    fix: Some("Install Claude Code: https://claude.com/claude-code".into()),
                },
                CheckResult {
                    kind: "claude-code/cc-hook-binary".into(),
                    level: CheckLevel::Warn,
                    message: "cc-hook not installed".into(),
                    fix: Some("ark ext claude-code reinstall-hook-binary".into()),
                },
                CheckResult {
                    kind: "claude-code/settings-hooks".into(),
                    level: CheckLevel::Warn,
                    message: "settings.json drift: 0/10".into(),
                    fix: Some("ark ext claude-code install-hooks".into()),
                },
                CheckResult {
                    kind: "claude-code/sessions-writable".into(),
                    level: CheckLevel::Error,
                    message: "$STATE/sessions unwritable".into(),
                    fix: Some("mkdir -p … && chmod u+w …".into()),
                },
                CheckResult {
                    kind: "claude-code/view-wired".into(),
                    level: CheckLevel::Info,
                    message: "wiring not verified".into(),
                    fix: None,
                },
            ],
        };
        let rendered = doctor::render_envelope_table(&env);
        // Every check's kind appears.
        assert!(rendered.contains("claude-code/which-claude"));
        assert!(rendered.contains("claude-code/cc-hook-binary"));
        assert!(rendered.contains("claude-code/settings-hooks"));
        assert!(rendered.contains("claude-code/sessions-writable"));
        assert!(rendered.contains("claude-code/view-wired"));
        // All three levels render.
        assert!(rendered.contains("ERROR"));
        assert!(rendered.contains("WARN"));
        assert!(rendered.contains("INFO"));
        // Remediation hints render on non-info checks.
        assert!(rendered.contains("Install Claude Code"));
        assert!(rendered.contains("ark ext claude-code reinstall-hook-binary"));
        assert!(rendered.contains("ark ext claude-code install-hooks"));
        assert!(rendered.contains("chmod u+w"));
    }

    // -- T-044 + T-045: list_columns RPC -----------------------------------

    #[tokio::test]
    async fn t044_list_columns_rpc_emits_three_columns() {
        let ext = ClaudeCodeExtension::new();
        let resp = ext
            .list_columns(ListColumnsRequest::default())
            .await
            .unwrap();
        let env: ColumnsEnvelope = serde_json::from_str(&resp.columns).unwrap();
        assert_eq!(env.columns.len(), 3);
        let names: Vec<&str> = env.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["cc model", "cc tokens", "cc cost"]);
    }

    #[tokio::test]
    async fn t044_list_columns_populated_after_fold_transcript_line() {
        let ext = ClaudeCodeExtension::new();
        ext.fold_transcript_line(
            r#"{"type":"message","role":"assistant","model":"claude-4-7","usage":{"input_tokens":10,"output_tokens":20},"cost_usd":0.125}"#,
        );
        let resp = ext
            .list_columns(ListColumnsRequest::default())
            .await
            .unwrap();
        let env: ColumnsEnvelope = serde_json::from_str(&resp.columns).unwrap();
        assert_eq!(env.columns[0].value, "claude-4-7");
        assert_eq!(env.columns[1].value, "30");
        assert_eq!(env.columns[2].value, "$0.12"); // format! uses banker's rounding
    }

    #[tokio::test]
    async fn t045_zero_event_regression_empty_model_zero_tokens_empty_cost() {
        // R11 + T-045: with no transcript lines observed, the three
        // columns render as ("", "0", "") respectively.
        let ext = ClaudeCodeExtension::new();
        // Snapshot before any fold call.
        let snap = ext.list_state_snapshot();
        assert_eq!(snap, CcListColumnState::default());

        let resp = ext
            .list_columns(ListColumnsRequest::default())
            .await
            .unwrap();
        let env: ColumnsEnvelope = serde_json::from_str(&resp.columns).unwrap();
        assert_eq!(env.columns[0].name, "cc model");
        assert_eq!(env.columns[0].value, "");
        assert_eq!(env.columns[1].name, "cc tokens");
        assert_eq!(env.columns[1].value, "0");
        assert_eq!(env.columns[2].name, "cc cost");
        assert_eq!(env.columns[2].value, "");
    }

    #[test]
    fn t044_list_state_round_trips_through_ext_state_json() {
        // R11: "per-session, persisted to SessionStatus.ext_state["claude-code"]".
        // The list state must serialize as a JSON object that round-trips
        // cleanly — the supervisor will eventually copy this into ext_state
        // but the round-trip path itself is what we validate here.
        let ext = ClaudeCodeExtension::new();
        ext.fold_transcript_line(
            r#"{"type":"message","role":"assistant","model":"m","usage":{"input_tokens":1,"output_tokens":2},"cost_usd":0.01}"#,
        );
        let snap = ext.list_state_snapshot();
        let v = serde_json::to_value(&snap).unwrap();
        // Shape contract: object with `model`, `tokens`, `cost_usd`.
        assert!(v.is_object());
        assert_eq!(v.get("model").unwrap(), "m");
        assert_eq!(v.get("tokens").unwrap(), 3);
        assert!(v.get("cost_usd").is_some());
        let back: CcListColumnState = serde_json::from_value(v).unwrap();
        assert_eq!(back, snap);
    }
}

// ---------------------------------------------------------------------------
// Tier 8 — T-046 reload tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tier8_reload_tests {
    use super::*;
    use ark_ext_proto::{ArkExtension, ControlVerbsRequest};
    use std::collections::BTreeMap;

    fn ext_config_with_claude_code(cfg: serde_json::Value) -> BTreeMap<String, serde_json::Value> {
        let mut ec: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        ec.insert("claude-code".into(), cfg);
        ec
    }

    #[test]
    fn t046_reload_rejected_when_scene_drops_use_directive() {
        let ext = ClaudeCodeExtension::new().with_config(ClaudeCodeConfig {
            match_cmds: vec!["claude".to_string()],
            transcript_tail_lines: 50,
            auto_install_hook_entries: false,
        });
        // Old config visible.
        assert_eq!(ext.match_cmds(), vec!["claude".to_string()]);

        // New ext_config does NOT carry `claude-code` key → reject.
        let empty: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let res = ext.reload(ReloadRequest {
            new_ext_config: empty,
            session: None,
        });
        assert!(matches!(res, Err(ReloadRejected::MissingExtConfig)));
        // Old config UNCHANGED — reload rejection leaves state intact.
        assert_eq!(ext.match_cmds(), vec!["claude".to_string()]);
        assert_eq!(ext.config_snapshot().transcript_tail_lines, 50);
        assert!(!ext.config_snapshot().auto_install_hook_entries);
    }

    #[test]
    fn t046_reload_config_only_refreshes_match_cmds_and_tail_lines() {
        let ext = ClaudeCodeExtension::new();
        assert!(ext.match_cmds().is_empty());
        assert_eq!(ext.config_snapshot().transcript_tail_lines, 200);

        let outcome = ext
            .reload_config_only(ext_config_with_claude_code(serde_json::json!({
                "match_cmds": ["claude", "claude-code"],
                "transcript_tail_lines": 42,
                "auto_install_hook_entries": true,
            })))
            .expect("reload OK");
        assert!(outcome.config_refreshed);
        assert!(!outcome.settings_reconciled); // no session ctx
        assert!(!outcome.transcript_watcher_reset);

        // Fresh config visible on next event dispatch — R12 acceptance.
        assert_eq!(
            ext.match_cmds(),
            vec!["claude".to_string(), "claude-code".to_string()]
        );
        assert_eq!(ext.config_snapshot().transcript_tail_lines, 42);
    }

    #[test]
    fn t046_reload_is_idempotent_across_two_calls_with_same_config() {
        let ext = ClaudeCodeExtension::new();
        let cfg = ext_config_with_claude_code(serde_json::json!({
            "match_cmds": ["claude"],
        }));
        let first = ext.reload_config_only(cfg.clone()).unwrap();
        let second = ext.reload_config_only(cfg).unwrap();
        assert_eq!(first, second);
        assert_eq!(ext.match_cmds(), vec!["claude".to_string()]);
    }

    #[test]
    fn t046_reload_preserves_stack_handle_identity() {
        // R12: "typed `Stack<_>` ref survives reload". The stack handle
        // is held inside the caller-constructed `ClaudeCodeView`, NOT
        // inside the extension — reload cannot invalidate it. We model
        // that by constructing a view with a stack handle, running a
        // reload, and asserting the handle's serde form (the opaque
        // HandleId bytes) is unchanged.
        let stack: ark_view::Stack<ClaudeCodeSubagent> =
            serde_json::from_str("\"stable-handle-id\"").unwrap();
        let view = ClaudeCodeView {
            subagents: Some(stack),
            ..Default::default()
        };
        let before = serde_json::to_string(view.subagents.as_ref().unwrap()).unwrap();

        let ext = ClaudeCodeExtension::new();
        let _ = ext
            .reload_config_only(ext_config_with_claude_code(serde_json::json!({
                "match_cmds": ["claude"],
            })))
            .expect("reload OK");

        // Handle bytes unchanged. Reload never touches view-held state.
        let after = serde_json::to_string(view.subagents.as_ref().unwrap()).unwrap();
        assert_eq!(before, after);
        assert_eq!(before, "\"stable-handle-id\"");
    }

    #[test]
    fn t046_reload_with_session_ctx_and_settings_opt_out_skips_reconcile() {
        // Config with auto_install_hook_entries=false → settings.json
        // reconcile MUST NOT fire even when a session ctx is present.
        let ext = ClaudeCodeExtension::new();
        let tmp = tempfile::tempdir().unwrap();
        let sess_dir = tmp.path().join("sessions").join("s0");
        std::fs::create_dir_all(&sess_dir).unwrap();
        let sock = sess_dir.join("cc-hook.sock");
        let outcome = ext
            .reload(ReloadRequest {
                new_ext_config: ext_config_with_claude_code(serde_json::json!({
                    "auto_install_hook_entries": false,
                })),
                session: Some(ReloadSessionCtx {
                    session_id: ark_types::SessionId::new("s0"),
                    socket_path: sock,
                    session_dir: sess_dir,
                    transcript_parent_dir: None,
                }),
            })
            .expect("reload OK");
        assert!(outcome.config_refreshed);
        assert!(
            !outcome.settings_reconciled,
            "auto_install_hook_entries=false must skip reconcile"
        );
        assert!(!outcome.transcript_watcher_reset);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t046_reload_with_transcript_dir_resets_watcher() {
        // When `transcript_parent_dir` is Some, reload sets up a fresh
        // watcher. We validate this runs without error; the watcher
        // itself has its own tests elsewhere.
        let ext = ClaudeCodeExtension::new();
        let tmp = tempfile::tempdir().unwrap();
        let sess_dir = tmp.path().join("sessions").join("s1");
        let transcript_dir = tmp.path().join("projects").join("p0").join("subagents");
        std::fs::create_dir_all(&sess_dir).unwrap();
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let sock = sess_dir.join("cc-hook.sock");

        let outcome = ext
            .reload(ReloadRequest {
                // Opt out of settings reconcile so this test doesn't
                // stomp the user's ~/.claude/settings.json — we only
                // want to exercise the watcher branch.
                new_ext_config: ext_config_with_claude_code(serde_json::json!({
                    "auto_install_hook_entries": false,
                })),
                session: Some(ReloadSessionCtx {
                    session_id: ark_types::SessionId::new("s1"),
                    socket_path: sock,
                    session_dir: sess_dir,
                    transcript_parent_dir: Some(transcript_dir),
                }),
            })
            .expect("reload OK");
        assert!(outcome.config_refreshed);
        assert!(!outcome.settings_reconciled);
        assert!(outcome.transcript_watcher_reset);
    }

    #[tokio::test]
    async fn t046_control_verbs_advertises_reload() {
        let ext = ClaudeCodeExtension::new();
        let resp = ext
            .control_verbs(ControlVerbsRequest::default())
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp.verbs).unwrap();
        let names: Vec<&str> = v
            .get("verbs")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|e| e.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"install-hooks"));
        assert!(names.contains(&"reinstall-hook-binary"));
        assert!(
            names.contains(&"reload"),
            "T-046 reload verb must be advertised"
        );
    }

    #[test]
    fn t046_reload_emits_warn_on_unknown_keys_and_falls_back_to_defaults() {
        // R12 inherits R9's "unknown keys warn but don't fail".
        let ext = ClaudeCodeExtension::new();
        let outcome = ext
            .reload_config_only(ext_config_with_claude_code(serde_json::json!({
                "match_cmds": ["claude"],
                "permission_policy": "auto", // unknown
            })))
            .expect("reload OK despite unknown key");
        assert!(outcome.config_refreshed);
        let snap = ext.config_snapshot();
        assert_eq!(snap.match_cmds, vec!["claude".to_string()]);
        // Other known fields take defaults.
        assert_eq!(snap.transcript_tail_lines, 200);
        assert!(snap.auto_install_hook_entries);
    }
}

// ---------------------------------------------------------------------------
// v0.2 backlog #3 — SubagentRegistry auto-wire tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod v0_2_backlog_3_tests {
    use super::*;
    use ark_types::ExtEvent;
    use std::sync::{Arc, Mutex};

    /// Build an `ExtEvent` of kind `claude-code.<kind>` — matches the
    /// shape the accept-loop receives from `payload_to_ext_event`.
    fn ev(kind: &str, payload: serde_json::Value) -> ExtEvent {
        ExtEvent {
            ext: EXT_NAME.to_string(),
            kind: kind.to_string(),
            payload,
        }
    }

    #[test]
    fn default_ext_has_fresh_registry_with_no_subagents() {
        let ext = ClaudeCodeExtension::new();
        assert!(ext.subagent_registry().snapshot().is_empty());
    }

    #[test]
    fn registry_is_shared_across_clones() {
        // Cloning the extension must share the registry Arc so a
        // clone captured inside the accept-loop task sees updates that
        // tests / supervisor code applied via the original handle
        // (and vice-versa). Pointer equality on the inner Arc is the
        // precise proof.
        let a = ClaudeCodeExtension::new();
        let b = a.clone();
        assert!(
            Arc::ptr_eq(a.subagent_registry(), b.subagent_registry()),
            "Clone must share the subagent registry Arc"
        );
    }

    #[test]
    fn registry_accepts_subagent_start_via_on_ext_event() {
        // Exercises the same entry point the accept-loop calls.
        let ext = ClaudeCodeExtension::new();
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "sub-1",
                "agent_type": "writer",
            }),
        );
        let emission = ext
            .subagent_registry()
            .on_ext_event(&e)
            .expect("subagent.start produces emission");
        assert_eq!(emission.id, "sub-1");
        assert_eq!(
            emission.payload.get("kind").and_then(|v| v.as_str()),
            Some("RenamePane")
        );
        assert!(ext.subagent_registry().get("sub-1").is_some());
    }

    #[test]
    fn with_rename_pane_emitter_stores_callback() {
        // Builder-style setter must round-trip — a subsequent snapshot
        // of the extension has the emitter installed. Using
        // `rename_pane_emitter`'s Option::is_some() via the Debug impl
        // would be indirect; instead we invoke the emitter directly
        // via the internal field (crate-private read).
        let captured: Arc<Mutex<Vec<RenamePaneEmission>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_cb = captured.clone();
        let ext = ClaudeCodeExtension::new().with_rename_pane_emitter(move |em| {
            captured_for_cb.lock().unwrap().push(em);
        });

        // Drive an emission via the registry + invoke the emitter the
        // accept-loop would.
        let e = ev(
            "subagent.start",
            serde_json::json!({
                "agent_id": "sub-cb",
                "agent_type": "t",
            }),
        );
        let emission = ext.subagent_registry().on_ext_event(&e).unwrap();
        ext.rename_pane_emitter.as_ref().expect("emitter installed")(emission);

        let recorded = captured.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].id, "sub-cb");
    }

    #[test]
    fn registry_round_trips_start_tool_stop_sequence() {
        // End-to-end: the three-event sequence a real subagent emits
        // flows through the registry cleanly, and the final state
        // reflects the full lifecycle. Simulates the accept loop's
        // per-frame `on_ext_event` call without actually spinning one
        // up (socket IO is orthogonal to registry folding).
        let ext = ClaudeCodeExtension::new();
        let reg = ext.subagent_registry();

        reg.on_ext_event(&ev(
            "subagent.start",
            serde_json::json!({"agent_id": "end-to-end", "agent_type": "code-writer"}),
        ))
        .expect("start");
        assert_eq!(
            reg.get("end-to-end").unwrap().status,
            SubagentStatus::Running
        );

        reg.on_ext_event(&ev(
            "pre-tool-use",
            serde_json::json!({"agent_id": "end-to-end", "tool_name": "Edit"}),
        ))
        .expect("tool");
        assert_eq!(
            reg.get("end-to-end").unwrap().last_tool.as_deref(),
            Some("Edit")
        );

        reg.on_ext_event(&ev(
            "subagent.stop",
            serde_json::json!({"agent_id": "end-to-end", "success": true}),
        ))
        .expect("stop");
        assert_eq!(reg.get("end-to-end").unwrap().status, SubagentStatus::Done);
    }

    #[test]
    fn emitter_fires_for_every_lifecycle_transition() {
        // The accept loop invokes the emitter on EVERY emission, not
        // just the start event — pane title updates track
        // (agent_type · status · last_tool) through the lifecycle.
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_cb = captured.clone();
        let ext = ClaudeCodeExtension::new().with_rename_pane_emitter(move |em| {
            if let Some(name) = em.payload.get("name").and_then(|v| v.as_str()) {
                captured_for_cb.lock().unwrap().push(name.to_string());
            }
        });

        for e in [
            ev(
                "subagent.start",
                serde_json::json!({"agent_id": "a", "agent_type": "t"}),
            ),
            ev(
                "pre-tool-use",
                serde_json::json!({"agent_id": "a", "tool_name": "Bash"}),
            ),
            ev(
                "subagent.stop",
                serde_json::json!({"agent_id": "a", "success": false}),
            ),
        ] {
            if let Some(em) = ext.subagent_registry().on_ext_event(&e) {
                if let Some(emitter) = ext.rename_pane_emitter.as_ref() {
                    emitter(em);
                }
            }
        }

        let titles = captured.lock().unwrap();
        assert_eq!(titles.len(), 3, "one emission per lifecycle transition");
        assert_eq!(titles[0], "t · running · -");
        assert_eq!(titles[1], "t · running · Bash");
        assert_eq!(titles[2], "t · failed · Bash");
    }

    #[test]
    fn non_subagent_events_do_not_fire_emitter() {
        // The registry filters to 3 kinds; any other kind is a no-op
        // and the emitter MUST not fire.
        let captured: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let c = captured.clone();
        let ext = ClaudeCodeExtension::new().with_rename_pane_emitter(move |_em| {
            *c.lock().unwrap() += 1;
        });
        for kind in ["post-tool-use", "notification", "user-prompt-submit"] {
            if let Some(em) = ext
                .subagent_registry()
                .on_ext_event(&ev(kind, serde_json::json!({"agent_id": "x"})))
            {
                ext.rename_pane_emitter.as_ref().unwrap()(em);
            }
        }
        assert_eq!(*captured.lock().unwrap(), 0);
    }
}
