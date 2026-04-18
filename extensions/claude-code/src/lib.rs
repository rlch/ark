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
    ArkExtension, ControlVerbsRequest, ControlVerbsResponse, ExtResult, OnSessionEndRequest,
    OnSessionEndResponse, OnSessionStartRequest, OnSessionStartResponse, OpaqueJson,
    SceneCompileHookRequest, SceneCompileHookResponse,
};
use ark_types::{CoreEvent, EventSink};
use async_trait::async_trait;
use tracing::{debug, info, warn};

pub mod hook_event;
pub mod hook_payload;
pub mod settings_json;
pub mod socket;
pub mod transcript;
pub mod view;

pub use hook_event::{HookEvent, UnknownHookEvent};
pub use hook_payload::{EXT_NAME, HookPayload, NdjsonLine, flat_event_name, payload_to_ext_event};
pub use settings_json::{
    ARK_MANAGED_KEY, CC_HOOK_BIN_NAME, DEFAULT_SETTINGS_REL_PATH, InstallOutcome, ReconcileOutcome,
    SettingsFile, SettingsJsonError, cc_hook_install_path, default_settings_path,
    install_cc_hook_at, install_cc_hook_default,
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
#[derive(Debug, Default, Clone)]
pub struct ClaudeCodeExtension {
    /// Optional broadcast sink the socket accept loop forwards decoded
    /// [`ark_types::ExtEvent`]s to, wrapped in
    /// [`CoreEvent::Ext`]. Cloneable — tokio broadcast senders are
    /// cheap to clone and safe to share across the spawned accept-loop
    /// task.
    event_sink: Option<EventSink>,

    /// T-032 (R5b): `match_cmds` config — list of raw `command cmd=<X>`
    /// values that should have `CLAUDE_HOOK_SOCKET=<session-sock-path>`
    /// injected into their pane env at scene-compile time. Default
    /// empty = the raw-cmd fallback is OFF (scene authors must opt in
    /// by setting `[claude-code] match_cmds = ["claude"]` in their
    /// extension config). This is the R5b fallback for scene authors
    /// who use the untyped `command cmd="claude"` form instead of the
    /// typed `claude-code` view (T-029). No typed subagent fan-out is
    /// provided by the fallback — see T-033 regression.
    match_cmds: Vec<String>,
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
            match_cmds: Vec::new(),
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
            match_cmds: Vec::new(),
        }
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
    #[must_use]
    pub fn with_match_cmds(mut self, cmds: Vec<String>) -> Self {
        self.match_cmds = cmds;
        self
    }

    /// Borrow the configured `match_cmds` list. Exposed primarily for
    /// tests that want to confirm the config injection took effect.
    pub fn match_cmds(&self) -> &[String] {
        &self.match_cmds
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
        reconcile_settings_for_session(
            &sid,
            sock.path(),
            sock.session_dir(),
            /* override_settings_path */ None,
            /* override_cc_hook_path */ None,
        );

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
        if self.match_cmds.is_empty() {
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
                if !self.match_cmds.iter().any(|c| c == cmd) {
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
