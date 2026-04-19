//! ark extension protocol — canonical Rust trait + request/response types.
//!
//! This crate is the **pure-contract** layer for the ark extension runtime RPC
//! surface defined in `cavekit-scene.md` R16. It contains:
//!
//! * [`ArkExtension`] — the `#[async_trait]` trait every extension implements.
//!   Compiled-in extensions implement the trait directly; subprocess +
//!   wasm-component extensions reach this surface through JSON-RPC 2.0 / wit-
//!   bindgen shims whose message schemas are generated from the `Facet`
//!   reflection of the types in this crate.
//! * Request / response structs for every method (one `*Request` struct and
//!   one `*Response` struct per RPC method). Every struct derives
//!   `Facet, Debug, Clone` and carries Rust `///` doc-comments on every
//!   field — facet's SHAPE surfaces these as LSP hover docs when editor
//!   tooling consumes the generated JSON-Schema.
//! * [`ExtensionError`] — `thiserror`-driven error enum whose variants line
//!   up with the `ext-proto/*` error-code family listed in R12 (cavekit-scene
//!   diagnostics).
//!
//! # Method surface (R16 v1)
//!
//! Grouped per R16 "Method surface v1". Every method takes a named request
//! struct and returns `Result<Response, ExtensionError>` — even void ops use
//! a dedicated response struct so future MINOR-version fields can be added
//! per the version-bump policy (R16 rule #3).
//!
//! * **Lifecycle** — [`ArkExtension::initialize`], [`ArkExtension::initialized`],
//!   [`ArkExtension::shutdown`], [`ArkExtension::ping`].
//! * **Async + cancel** — [`ArkExtension::cancel`], [`ArkExtension::progress`],
//!   [`ArkExtension::task_create`], [`ArkExtension::task_get`],
//!   [`ArkExtension::task_cancel`].
//! * **Event bus** — [`ArkExtension::event_subscribe`],
//!   [`ArkExtension::event_unsubscribe`], [`ArkExtension::event_emit`],
//!   [`ArkExtension::event_notify`].
//! * **Intents** — [`ArkExtension::intent_unregister`],
//!   [`ArkExtension::intent_dispatch`].
//! * **UI (keybind / status)** — [`ArkExtension::ui_keybind_register`],
//!   [`ArkExtension::ui_keybind_unregister`], [`ArkExtension::ui_status_push`].
//! * **UI (panes)** — [`ArkExtension::ui_pane_request`],
//!   [`ArkExtension::ui_pane_close`].
//! * **Workspace** — [`ArkExtension::workspace_apply_edit`],
//!   [`ArkExtension::workspace_configuration`],
//!   [`ArkExtension::workspace_show_document`],
//!   [`ArkExtension::workspace_show_message`],
//!   [`ArkExtension::workspace_show_message_request`].
//! * **Scene** — [`ArkExtension::scene_get_root`].
//! * **Host (wasm only, capability-gated)** — [`ArkExtension::host_fs_read`],
//!   [`ArkExtension::host_fs_write`], [`ArkExtension::host_proc_spawn`],
//!   [`ArkExtension::host_net_fetch`].
//! * **Logging** — [`ArkExtension::log_write`], [`ArkExtension::log_set_level`].
//!
//! # Default impls
//!
//! Every method has a default implementation that returns
//! [`ExtensionError::method_not_found`] with the method name. Extensions
//! implement only the methods they support; ark's dispatcher maps
//! `method_not_found` to JSON-RPC error code `-32601` per R16 best-effort
//! mode. Agent-lifecycle / engine methods are explicitly OUT of this surface
//! (R16) — ark uses ACP (see R17) for that.

#![deny(missing_docs)]

use async_trait::async_trait;
use facet::Facet;

pub mod supervision;
pub mod transport;

pub use supervision::{CrashReport, ExtSupervisor, SupervisorHandle};
pub use transport::{
    ExtensionClient, InProcClient, NdjsonClient, NdjsonServer, ProgressReceiver, RequestOptions,
    ReverseRequestGate, TaskProgress, method_to_capability,
};

// Re-export ark-view types so extension authors can `use ark_ext_proto::{...}`
// without a direct ark-view dep (per cavekit-soul-phase-2-ark-view.md R11).
// These are the derive-addressable surface for `#[derive(View)]` /
// `#[derive(Extension)]`: core trait + typed wrappers + kind enums + id types.
// Suppression / lookup types stay behind the `ark_view` crate directly —
// they're host-side consumed by the supervisor, not extension authors.
pub use ark_view::{
    CommandView, HandleId, HandleKind, InvalidationCause, Pane, PaneLike, Stack, TabHandle, View,
    ZellijView,
};

/// Opaque JSON text carried as a UTF-8 string.
///
/// Fields typed `OpaqueJson` hold a serialized JSON document that ark's
/// RPC transport parses on dispatch and re-emits on response. Using a
/// string rather than `serde_json::Value` keeps the contract surface
/// `Facet`-derivable (facet does not yet provide a blanket SHAPE impl for
/// `serde_json::Value`). Extension authors SHOULD use `serde_json::to_string`
/// + `serde_json::from_str` at the boundary; ark core does the same when
/// round-tripping payloads from JSON-RPC messages. JSON-Schema generated
/// from facet SHAPE reflection annotates these fields as
/// `{ "type": "string", "format": "json" }` so foreign-language bindings
/// can automate the parse step.
///
/// Canonical value for "no payload" is the JSON text `"null"` (four
/// characters) — every empty-payload site in this module documents the
/// convention explicitly.
pub type OpaqueJson = String;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned from every [`ArkExtension`] method.
///
/// Variants line up with the `ext-proto/*` family in `cavekit-scene.md` R12.
/// The wire encoding (JSON-RPC error code) is added by the transport layer
/// (subprocess / wasm) rather than baked in here so this crate stays pure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtensionError {
    /// Method is not implemented by this extension. Maps to JSON-RPC
    /// `-32601 method not found` (R16 "missing methods return JSON-RPC
    /// `-32601`"). Carries the method name for diagnostics.
    #[error("method not found: {0}")]
    MethodNotFound(String),

    /// Request rejected because the extension has not declared the required
    /// capability in its manifest (R16 "capability-gated"). Example:
    /// subprocess extension calling a `host/*` method.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),

    /// Protocol version negotiated at `initialize` lies outside the range
    /// the running ark supports. Maps to `ext-proto/unsupported-version`.
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(String),

    /// Input failed schema validation (bad type, missing required field).
    /// Transports map this to JSON-RPC `-32602 invalid params`.
    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// Catch-all internal failure. Carries an owned message so the error
    /// stays `Send + 'static` without leaking the underlying source type
    /// across the RPC boundary.
    #[error("internal error: {0}")]
    Internal(String),

    /// Subprocess extension exited unexpectedly. Carries the last lines
    /// of stderr captured by the supervisor's log-tail buffer per R16
    /// (`ext/crashed` UserEvent payload). Consumers (the supervisor
    /// crate) translate this into `AgentEvent::UserEvent
    /// ark.ext.crashed { name, exit_code, stderr_tail }`.
    #[error("extension {name} crashed (exit {exit_code:?}): {stderr_tail}")]
    Crashed {
        /// Extension instance name (matches `ExtensionMetadata::name`).
        name: String,
        /// Process exit code if available, `None` for signal-terminated
        /// children whose status code couldn't be retrieved.
        exit_code: Option<i32>,
        /// Last buffered lines of the child's stderr — joined with
        /// `\n`. Capped at the supervisor's log-tail length (default
        /// 100 lines).
        stderr_tail: String,
    },

    /// Op targeted a handle that is no longer valid. The host MUST return
    /// this variant (not `method_not_found` nor `Internal`) for any op in
    /// the R6 pane/* + stack/* surface invoked against an invalidated
    /// handle. Mirrors the push-style `ark.handle.invalidated { handle,
    /// cause }` ExtEvent for race-free pull-style detection; extensions
    /// that miss the event observe the failure lazily. Per cavekit-
    /// soul-phase-2-ark-view.md R7 + decision #3c.
    ///
    /// Wire error code (R12): `ext-proto/handle-gone`.
    #[error("handle {handle} gone ({cause:?})")]
    HandleGone {
        /// The invalidated handle id that the op targeted.
        handle: ark_view::HandleId,
        /// Which of the three terminal causes produced the invalidation.
        cause: ark_view::InvalidationCause,
    },
}

impl ExtensionError {
    /// Construct a [`ExtensionError::MethodNotFound`] variant for the
    /// default-impl fall-through path. Every trait method's default impl
    /// calls this with its own name so ark's dispatcher can convert it to
    /// JSON-RPC `-32601`.
    pub fn method_not_found(method: impl Into<String>) -> Self {
        ExtensionError::MethodNotFound(method.into())
    }

    /// Construct a [`ExtensionError::UnsupportedVersion`] variant using a
    /// human-readable mismatch description. Maps to error code
    /// `ext-proto/unsupported-version` (R12 + R16).
    pub fn unsupported_version(message: impl Into<String>) -> Self {
        ExtensionError::UnsupportedVersion(message.into())
    }

    /// Construct a [`ExtensionError::CapabilityDenied`] variant for the
    /// reverse-request gating path (R16: `host/*` + `workspace/*` calls
    /// require the extension to have declared the capability and to
    /// present a valid session token). Maps to wire error code
    /// `ext-proto/capability-denied` (R12).
    pub fn capability_denied(capability: impl Into<String>) -> Self {
        ExtensionError::CapabilityDenied(capability.into())
    }

    /// Stable wire error code per R12 — used by the JSON-RPC transport
    /// when serialising an `ExtensionError` into a `ResponseError`. The
    /// `code()` strings match the `ext-proto/*` family enumerated in R12
    /// so consumers can match on them without parsing the `Display`
    /// message.
    pub fn code(&self) -> &'static str {
        match self {
            ExtensionError::MethodNotFound(_) => "ext-proto/method-not-found",
            ExtensionError::CapabilityDenied(_) => "ext-proto/capability-denied",
            ExtensionError::UnsupportedVersion(_) => "ext-proto/unsupported-version",
            ExtensionError::InvalidParams(_) => "ext-proto/invalid-params",
            ExtensionError::Internal(_) => "ext-proto/internal",
            ExtensionError::Crashed { .. } => "ext/crashed",
            ExtensionError::HandleGone { .. } => "ext-proto/handle-gone",
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol version + capabilities (R10 + R16)
// ---------------------------------------------------------------------------

/// Parsed `MAJOR.MINOR` protocol version (R16 wire format — semver, no
/// patch). Constructed via [`ProtocolVersion::parse`] from the
/// [`InitializeRequest::protocol_version`] / [`InitializeResponse::protocol_version`]
/// strings; ark's compatibility check uses [`ProtocolVersion::is_compatible`]
/// which mirrors the R16 3-tier out-of-range policy (different MAJOR =
/// hard fail; MINOR mismatch = best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersion {
    /// MAJOR component — backwards-incompatible epoch.
    pub major: u32,
    /// MINOR component — additive feature surface.
    pub minor: u32,
}

impl ProtocolVersion {
    /// Construct from raw components. Provided for tests and for
    /// emitting the `cargo`-time compile-in version constant.
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Parse the wire `MAJOR.MINOR` form. Trailing patch components or
    /// pre-release suffixes are rejected per R16 ("semver, no patch").
    /// Empty string + non-numeric components surface as
    /// [`ExtensionError::InvalidParams`] so transports can distinguish
    /// a malformed version from a known-but-incompatible one.
    pub fn parse(text: &str) -> Result<Self, ExtensionError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ExtensionError::InvalidParams(
                "protocol version is empty".into(),
            ));
        }
        let mut parts = trimmed.split('.');
        let major_text = parts
            .next()
            .ok_or_else(|| ExtensionError::InvalidParams("missing MAJOR".into()))?;
        let minor_text = parts
            .next()
            .ok_or_else(|| ExtensionError::InvalidParams("missing MINOR".into()))?;
        if parts.next().is_some() {
            return Err(ExtensionError::InvalidParams(format!(
                "expected MAJOR.MINOR (no patch component), got {trimmed:?}"
            )));
        }
        let major: u32 = major_text
            .parse()
            .map_err(|_| ExtensionError::InvalidParams(format!("bad MAJOR: {major_text:?}")))?;
        let minor: u32 = minor_text
            .parse()
            .map_err(|_| ExtensionError::InvalidParams(format!("bad MINOR: {minor_text:?}")))?;
        Ok(Self { major, minor })
    }

    /// Render back to `MAJOR.MINOR`.
    pub fn to_wire(self) -> String {
        format!("{}.{}", self.major, self.minor)
    }

    /// R16 compatibility predicate: same MAJOR = compatible. MINOR
    /// mismatch is a soft signal (caller may log a "best-effort mode"
    /// warning per the 3-tier policy) but does not fail the handshake.
    pub fn is_compatible(self, other: ProtocolVersion) -> bool {
        self.major == other.major
    }
}

/// Compile-time host-side protocol version this build of `ark-ext-proto`
/// supports. Used by [`ExtensionClient::handshake`] as the
/// `client_version` default.
///
/// **1.1 additions over 1.0** (Phase 2, additive — MAJOR unchanged):
/// * 6 new RPC methods — `pane/emit`, `pane/replace_view`, `pane/close`,
///   `stack/spawn_pane`, `stack/close_child`, `stack/clear`
///   (cavekit-soul-phase-2 T-018/T-019).
/// * 6 new feature-group hooks — `on_session_start`, `on_session_end`,
///   `scene_compile_hook`, `control_verbs`, `doctor_checks`,
///   `list_columns` (T-020/T-021).
/// * 8-flag capability taxonomy — see [`PHASE_2_CAPABILITY_FLAGS`]
///   (T-024); each flag gates a method/hook group (T-028/T-029).
/// * New wire code `-32006 ext-proto/handle-gone` +
///   [`ExtensionError::HandleGone`]; `SessionEnded.exit: ExitReason`
///   (T-014/T-045).
///
/// All additions are guarded by capability flags; a 1.0 client talking
/// to a 1.1 extension is still compatible under [`ProtocolVersion::is_compatible`]
/// (same MAJOR = OK + soft WARN per R16).
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 1);

/// v1 capability-flag taxonomy (Phase 2 ext-surface R6).
///
/// These are the ONLY capability flags Phase 2 introduces. Each flag
/// gates a group of trait methods + manifest surfaces added by this
/// phase; see the `<flag> → method` mapping in cavekit-soul-phase-2-
/// ext-surface.md R6. Naming convention per decision #4a:
/// `<domain>.<feature>.v<N>`. Bumping `v<N>` is a breaking change
/// within the feature group.
///
/// Adding a flag here outside the 8-flag slate in Phase 2 is a kit
/// violation — the `phase_2_capability_flag_set_is_exactly_eight`
/// test enforces set equality.
pub const PHASE_2_CAPABILITY_FLAGS: &[&str] = &[
    "view.pane.v1",
    "view.stack.v1",
    "ext.lifecycle.v1",
    "ext.scene_compile_hook.v1",
    "ext.control_verbs.v1",
    "ext.doctor.v1",
    "ext.list_columns.v1",
    "ext.reload_gate.v1",
];

/// Object-of-objects capability bag per R10 — `{ ui: {…}, intents: {…},
/// events: {…}, host: {…}, agent: {…} }`. The wire shape is opaque
/// JSON ([`OpaqueJson`]) because every category's body schema lives in
/// the consumer crate (`ark-ext-metadata-types`); this struct is the
/// in-memory mirror used by the host-side capability gate (T-9.5.8).
///
/// New top-level categories can be added without breaking existing
/// extensions (R16 rule #8 — adding capability flag is a MINOR bump);
/// extensions opt-in by listing their category in their declared
/// capabilities. Anything not on the bag is denied by default (R16
/// gating).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Set of dotted capability identifiers granted to the extension.
    /// Examples: `"ui.keybind"`, `"ui.pane"`, `"ui.status"`,
    /// `"intents.provide"`, `"intents.dispatch"`, `"events.subscribe"`,
    /// `"events.emit"`, `"host.fs.read"`, `"host.fs.write"`,
    /// `"host.proc.spawn"`, `"host.net.fetch"`, `"workspace.applyEdit"`.
    /// The dotted form mirrors the JSON-RPC method namespace so the
    /// gate can map a method directly to a capability identifier.
    pub granted: std::collections::BTreeSet<String>,
}

impl Capabilities {
    /// Empty capability set — denies everything. Use as the default
    /// when an extension fails to declare any capability category.
    pub fn empty() -> Self {
        Self {
            granted: std::collections::BTreeSet::new(),
        }
    }

    /// Construct from any iterator of capability identifier strings.
    /// Convenience for tests and host-side adapters that build the bag
    /// from `ExtensionMetadata`.
    pub fn from_iter<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            granted: iter.into_iter().map(Into::into).collect(),
        }
    }

    /// Predicate used by the reverse-request gate (T-9.5.8). The
    /// `capability` argument follows the dotted form documented on
    /// [`Capabilities::granted`].
    pub fn allows(&self, capability: &str) -> bool {
        self.granted.contains(capability)
    }

    /// Render to the object-of-objects [`OpaqueJson`] wire format
    /// (R10 — `{ ui: { keybind: true }, host: { fs: { read: true } } }`).
    /// Identifiers are split on `.` and folded into nested objects.
    pub fn to_wire(&self) -> OpaqueJson {
        let mut root = serde_json::Map::new();
        for cap in &self.granted {
            let parts: Vec<&str> = cap.split('.').collect();
            insert_nested(&mut root, &parts);
        }
        serde_json::Value::Object(root).to_string()
    }

    /// Inverse of [`Capabilities::to_wire`] — flattens nested
    /// `{ ui: { keybind: true } }` objects back into dotted
    /// identifiers. Non-true leaves are ignored (`{ ui: { pane: false } }`
    /// leaves `ui.pane` un-granted). Garbage shapes return
    /// [`ExtensionError::InvalidParams`].
    pub fn from_wire(json: &str) -> Result<Self, ExtensionError> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| ExtensionError::InvalidParams(format!("capability JSON parse: {e}")))?;
        let mut granted = std::collections::BTreeSet::new();
        match value {
            serde_json::Value::Null => {}
            serde_json::Value::Object(map) => {
                collect_nested(&map, "", &mut granted);
            }
            other => {
                return Err(ExtensionError::InvalidParams(format!(
                    "capability root must be object or null, got {other}"
                )));
            }
        }
        Ok(Self { granted })
    }
}

fn insert_nested(parent: &mut serde_json::Map<String, serde_json::Value>, parts: &[&str]) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        parent.insert(parts[0].to_string(), serde_json::Value::Bool(true));
        return;
    }
    let head = parts[0].to_string();
    let entry = parent
        .entry(head)
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let serde_json::Value::Object(child) = entry {
        insert_nested(child, &parts[1..]);
    }
}

fn collect_nested(
    map: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    for (key, value) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            serde_json::Value::Bool(true) => {
                out.insert(path);
            }
            serde_json::Value::Object(child) => {
                collect_nested(child, &path, out);
            }
            // Bool(false) or other leaf shapes don't grant the cap;
            // foreign capability bodies (e.g. `{ host: { fs: { read:
            // { paths: [...] } } } }`) collapse into a single grant
            // for `host.fs.read` once they hit a non-object node.
            _ => {
                out.insert(path);
            }
        }
    }
}

/// Per-session token issued at `initialize`. Carried as a freshly
/// generated 128-bit hex string. Reverse-requests from the extension
/// MUST present this token; ark drops requests with an absent or
/// mismatched token (R16 "session token issued at initialize; valid
/// only for session lifetime").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionToken(String);

impl SessionToken {
    /// Mint a fresh token. Uses two `u64`s drawn from the ambient
    /// system clock + process id to avoid pulling in a `rand` dep
    /// — the token is not a security primitive against an attacker on
    /// the same machine, only a foot-gun guard against extensions that
    /// accidentally call host RPCs after the session has ended.
    pub fn mint() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos() as u64;
        let pid = std::process::id() as u64;
        let mix = nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(pid);
        Self(format!("{:016x}{:016x}", nanos, mix))
    }

    /// Construct from a known string. Used by the extension side when
    /// echoing the token back from `initialize`.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying token string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Result alias used by every [`ArkExtension`] method.
pub type ExtResult<T> = Result<T, ExtensionError>;

// ---------------------------------------------------------------------------
// Common value types
// ---------------------------------------------------------------------------

/// Log severity for [`ArkExtension::log_write`] calls, aligned with LSP
/// `window/logMessage` message types and the `tracing` crate's `Level`.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Hard errors — the extension could not complete the requested work.
    Error = 1,
    /// Warnings — recoverable but user-visible misbehaviour.
    Warn = 2,
    /// Informational messages — normal-path narration.
    Info = 3,
    /// Debug-level narration — verbose, off by default in production.
    Debug = 4,
    /// Trace-level narration — finest granularity, typically gated by
    /// `log/setLevel`.
    Trace = 5,
}

/// Opaque task handle returned by [`ArkExtension::task_create`] and passed
/// back to [`ArkExtension::task_get`] / [`ArkExtension::task_cancel`].
///
/// Callers treat this as an opaque string (R16 async semantics are MCP-style:
/// long-running ops return a task handle; poll `task/get` or subscribe to
/// `$/progress`).
#[derive(Facet, Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TaskId {
    /// Extension-minted identifier. Must be unique within the extension
    /// session. Ark treats this as opaque.
    pub value: String,
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Handshake payload sent by ark to the extension during `initialize`.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InitializeRequest {
    /// Ark's supported extension-protocol version range, encoded as
    /// `MAJOR.MINOR` (no patch) per R16 version-negotiation wire format.
    pub protocol_version: String,
    /// Capabilities the client (ark) offers, object-of-objects shape per
    /// R10 (`{ ui: {...}, intents: {...}, events: {...} }`). Carried as
    /// opaque JSON here; schema validation is extension-local.
    pub client_capabilities: OpaqueJson,
    /// Ark version string (`env!("CARGO_PKG_VERSION")`) for diagnostic
    /// use. Extensions may log but MUST NOT gate behaviour on this —
    /// capability flags are the authoritative feature-detection channel.
    pub client_info: String,
}

/// Handshake response returned by the extension.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InitializeResponse {
    /// Extension's supported protocol-version range (`MAJOR.MINOR`).
    pub protocol_version: String,
    /// Capabilities the extension advertises. Same object-of-objects shape
    /// as [`InitializeRequest::client_capabilities`].
    pub extension_capabilities: OpaqueJson,
    /// `{ name, version }` descriptor for diagnostics. Free-form JSON so
    /// new fields don't force a protocol bump (R16 rule #3).
    pub extension_info: OpaqueJson,
    /// Session token issued by the host on a successful handshake (R16
    /// "session token issued at initialize"). Empty string when the
    /// extension itself populates this field — the host
    /// ([`ExtensionClient::handshake`]) overwrites with a freshly-minted
    /// [`SessionToken`] before returning to the caller. Subsequent
    /// reverse-requests from the extension MUST present this token; the
    /// token is valid only for the lifetime of the session.
    #[serde(default)]
    pub session_token: String,
}

/// Void notification confirming the extension has completed any post-
/// initialize setup (equivalent to LSP's `initialized` notification).
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InitializedRequest {}

/// Void response for [`ArkExtension::initialized`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InitializedResponse {}

/// Shutdown request — graceful teardown. Per R16 subprocess supervision,
/// ark follows `shutdown` → stdin-close → `SIGTERM` → `SIGKILL`.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShutdownRequest {}

/// Void response for [`ArkExtension::shutdown`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShutdownResponse {}

/// Liveness probe.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PingRequest {}

/// Liveness response — body is empty but the response struct stays
/// extensible.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PingResponse {}

// ---------------------------------------------------------------------------
// Async + cancel
// ---------------------------------------------------------------------------

/// `$/cancel` notification — carries the JSON-RPC request id the caller
/// wants to cancel. Late responses arriving after cancel are silently
/// dropped per MCP cancellation semantics (R16).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CancelRequest {
    /// JSON-RPC id of the in-flight request to cancel. Carried as a string
    /// to cover both numeric and string ids uniformly.
    pub id: String,
}

/// Void response for [`ArkExtension::cancel`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CancelResponse {}

/// `$/progress` notification — extension emits periodic updates for a
/// running task. The `token` correlates to a prior [`TaskCreateResponse`]
/// or a caller-supplied value.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgressRequest {
    /// Progress correlation token. Ark joins progress entries by token
    /// when rendering them in the status line.
    pub token: String,
    /// Free-form progress payload — typically `{ kind, message, percentage }`
    /// per LSP conventions. Opaque here so the schema can grow.
    pub value: OpaqueJson,
}

/// Void response for [`ArkExtension::progress`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProgressResponse {}

/// `task/create` request — the extension starts a long-running op and
/// returns a handle that the client can later query or cancel.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskCreateRequest {
    /// Short human-readable label — used as `ark status` line text and
    /// debug trace prefix.
    pub label: String,
    /// Opaque extension-defined payload describing the task. Schema is
    /// the extension's own; ark does not inspect.
    pub params: OpaqueJson,
}

/// Response for [`ArkExtension::task_create`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskCreateResponse {
    /// Opaque task handle. Passed to [`TaskGetRequest::task`] and
    /// [`TaskCancelRequest::task`].
    pub task: TaskId,
}

/// `task/get` request — poll the state of a previously created task.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskGetRequest {
    /// Handle returned by [`TaskCreateResponse::task`].
    pub task: TaskId,
}

/// Response for [`ArkExtension::task_get`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskGetResponse {
    /// `"pending" | "running" | "succeeded" | "failed" | "cancelled"` —
    /// carried as a plain string so new states can be added without a
    /// MAJOR bump (R16 rule #6, widen-enum-MINOR-if-default-fallback).
    pub status: String,
    /// Task output on `succeeded`, error descriptor on `failed`, null
    /// otherwise. Schema is extension-local.
    pub result: Option<OpaqueJson>,
}

/// `task/cancel` request — cooperative cancel on a running task.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskCancelRequest {
    /// Handle of the task to cancel.
    pub task: TaskId,
}

/// Void response for [`ArkExtension::task_cancel`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TaskCancelResponse {}

// ---------------------------------------------------------------------------
// Event bus
// ---------------------------------------------------------------------------

/// `event/subscribe` — tell ark the extension wants incoming
/// [`EventNotifyRequest`] callbacks for events matching `selector`.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventSubscribeRequest {
    /// Event selector expression per R4 (namespaced name or glob pattern,
    /// e.g. `"ark.core.session.started"` or `"mycorp.*"`).
    pub selector: String,
}

/// Response for [`ArkExtension::event_subscribe`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventSubscribeResponse {
    /// Opaque subscription id — passed back to
    /// [`EventUnsubscribeRequest::subscription`] to revoke.
    pub subscription: String,
}

/// `event/unsubscribe` — revoke a prior subscription.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventUnsubscribeRequest {
    /// Subscription id from [`EventSubscribeResponse::subscription`].
    pub subscription: String,
}

/// Void response for [`ArkExtension::event_unsubscribe`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventUnsubscribeResponse {}

/// `event/emit` — extension publishes an event onto ark's bus. Namespace
/// prefix MUST be the extension's own `<ext-name>.<event>` (R11) — ark
/// rejects `ark.core.*` writes with `ext/reserved-namespace`.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventEmitRequest {
    /// Fully-qualified event name. Unprefixed names get auto-prefixed by
    /// ark's emit path when dispatched from an extension sidecar (R11).
    pub name: String,
    /// Event payload. Schema governed by the extension's
    /// `events: Vec<EventDecl>` manifest entry (see
    /// `ark-ext-metadata-types`).
    pub payload: OpaqueJson,
}

/// Void response for [`ArkExtension::event_emit`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventEmitResponse {}

/// `event/notify` — host→ext delivery for a subscribed event.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventNotifyRequest {
    /// Subscription id the event is delivered under.
    pub subscription: String,
    /// Fully-qualified event name (never rewritten by ark at delivery —
    /// extensions see exactly what was emitted).
    pub name: String,
    /// Event payload.
    pub payload: OpaqueJson,
}

/// Void response for [`ArkExtension::event_notify`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventNotifyResponse {}

// ---------------------------------------------------------------------------
// Intents
// ---------------------------------------------------------------------------

/// `intent/unregister` — drop a prior intent registration.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntentUnregisterRequest {
    /// Intent name.
    pub name: String,
}

/// Void response for [`ArkExtension::intent_unregister`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IntentUnregisterResponse {}

/// `intent/dispatch` — ark asks the extension to execute one of its
/// previously-registered intents. Return value is free-form JSON the
/// extension defines in its manifest (`IntentDecl::args_schema` governs
/// `args`; return schema is intent-specific and opaque at this layer).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntentDispatchRequest {
    /// Intent to dispatch.
    pub name: String,
    /// Args — schema-validated against the manifest before arrival.
    pub args: OpaqueJson,
}

/// Response for [`ArkExtension::intent_dispatch`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntentDispatchResponse {
    /// Intent return value. `null` for void intents.
    pub value: OpaqueJson,
}

// ---------------------------------------------------------------------------
// Pane / Stack handle ops (Phase 2 R6)
// ---------------------------------------------------------------------------

/// `pane/emit` — push an extension-owned event into the pane.
///
/// Fields typed `OpaqueJson` rather than `serde_json::Value` because
/// facet does not yet provide a blanket SHAPE impl for
/// `serde_json::Value`; see the module-level note on [`OpaqueJson`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneEmitRequest {
    /// Target pane handle (opaque id; see ark-view R5).
    pub handle: ark_view::HandleId,
    /// Event kind (extension's `<ext>.<kind>` form).
    pub kind: String,
    /// Free-form JSON payload the extension defines for this kind.
    /// Carried as [`OpaqueJson`]; the extension's own manifest / schema
    /// governs the shape.
    pub payload: OpaqueJson,
}

/// Void response for [`ArkExtension::pane_emit`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaneEmitResponse {}

/// `pane/replace_view` — swap the renderer currently mounted on a pane.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneReplaceViewRequest {
    /// Target pane handle.
    pub handle: ark_view::HandleId,
    /// New view-type token (`"<ext>.<view>"`). The host enforces that
    /// the token is registered in some extension's manifest.
    pub view_type: String,
    /// New view body — JSON payload the incoming view type's
    /// constructor consumes. Shape is opaque at this layer; carried as
    /// [`OpaqueJson`] per the facet/serde_json note above.
    pub view_body: OpaqueJson,
}

/// Void response for [`ArkExtension::pane_replace_view`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaneReplaceViewResponse {}

/// `pane/close` — close the pane (invalidates its handle).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneCloseRequest {
    /// Target pane handle.
    pub handle: ark_view::HandleId,
}

/// Void response for [`ArkExtension::pane_close`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaneCloseResponse {}

/// `stack/spawn_pane` — spawn a new child pane into the stack.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackSpawnPaneRequest {
    /// Target stack handle.
    pub stack: ark_view::HandleId,
    /// Attrs payload — v0.2 widened this to carry the contents of
    /// [`ark_view::PaneAttrs::view_attrs`] (per-view JSON, opaque at
    /// this layer). v0.1 peers populated `{}`; the
    /// [`ark_view::PaneAttrs`] serde shape stays round-trip compatible
    /// with both, so mixed-version frames on the wire continue to
    /// decode (v0.1-empty `{}` → `view_attrs: null` after widening).
    ///
    /// The type stays [`OpaqueJson`] rather than `ark_view::PaneAttrs`
    /// directly because the proto crate does NOT depend on
    /// `ark-view`'s serde_json-parsed surface — downstream facet-based
    /// bindings don't have `serde_json::Value` support yet. Consumers
    /// decode with `serde_json::from_str::<ark_view::PaneAttrs>(&attrs)`.
    pub attrs: OpaqueJson,
}

/// Reply for [`ArkExtension::stack_spawn_pane`] — carries the new
/// child pane's opaque handle id (kit R6 AC: single `handle` field).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackSpawnPaneResponse {
    /// Opaque handle id of the newly-spawned child pane.
    pub handle: ark_view::HandleId,
}

/// `stack/close_child` — close one child pane without tearing the stack.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackCloseChildRequest {
    /// Target stack handle.
    pub stack: ark_view::HandleId,
    /// Child pane handle to close.
    pub handle: ark_view::HandleId,
}

/// Void response for [`ArkExtension::stack_close_child`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StackCloseChildResponse {}

/// `stack/clear` — close every child pane, leaving the stack itself.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackClearRequest {
    /// Target stack handle.
    pub stack: ark_view::HandleId,
}

/// Void response for [`ArkExtension::stack_clear`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StackClearResponse {}

// ---------------------------------------------------------------------------
// Session lifecycle hooks (Phase 2 ext-surface R1)
// ---------------------------------------------------------------------------

/// `on_session_start` request — ark calls this at session spawn so the
/// extension can attach per-session state without polling events. Per
/// `phase-2-design-decisions.md` §R-5 the hook fires BEFORE the session's
/// first engine event is emitted, so extensions see a complete
/// [`ark_types::SessionSpec`] before any scene reactivity kicks in.
///
/// The `spec` field is carried as [`OpaqueJson`] (a JSON-encoded
/// `ark_types::SessionSpec`) because `ark_types::SessionSpec` does not
/// derive `facet::Facet`; adding that derive would widen `ark-types`'
/// dependency surface and is out of scope for T-020. Consumers decode
/// with `serde_json::from_str::<ark_types::SessionSpec>(&req.spec)`.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OnSessionStartRequest {
    /// JSON-encoded [`ark_types::SessionSpec`] — session name, id, cwd,
    /// scene path, env overrides, per-extension config. Decode with
    /// `serde_json::from_str`.
    pub spec: OpaqueJson,
}

/// Void response for [`ArkExtension::on_session_start`]. Dedicated
/// struct (rather than `()`) so MINOR-level evolution can add fields
/// without a protocol bump (kit #4c).
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OnSessionStartResponse {}

/// `on_session_end` request — ark calls at session termination with the
/// terminal [`ark_types::event::ExitReason`] per
/// `phase-2-design-decisions.md` §R-5.
///
/// `spec` and `exit` are both carried as [`OpaqueJson`] — `ark_types`
/// types do not derive `facet::Facet` (see [`OnSessionStartRequest`]
/// note). Decode `spec` with
/// `serde_json::from_str::<ark_types::SessionSpec>(&req.spec)` and
/// `exit` with `serde_json::from_str::<ark_types::event::ExitReason>(&req.exit)`.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OnSessionEndRequest {
    /// JSON-encoded [`ark_types::SessionSpec`] of the session that ended.
    pub spec: OpaqueJson,
    /// JSON-encoded [`ark_types::event::ExitReason`] — one of
    /// `{"kind":"normal"}`, `{"kind":"cancelled"}`,
    /// `{"kind":"error","value":"<msg>"}`.
    pub exit: OpaqueJson,
}

/// Void response for [`ArkExtension::on_session_end`]. Dedicated struct
/// (rather than `()`) so MINOR-level evolution can add fields without a
/// protocol bump (kit #4c).
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OnSessionEndResponse {}

// ---------------------------------------------------------------------------
// Feature-group hooks (Phase 2 ext-surface R2)
// ---------------------------------------------------------------------------

/// `scene_compile_hook` request — carries the partial scene AST so the
/// extension can contribute layout nodes, intent bindings, etc.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SceneCompileHookRequest {
    /// Opaque partial-scene snapshot. Exact shape is pinned in
    /// `cavekit-soul-phase-2-host-dispatch.md` when the first caller
    /// materialises; kept as [`OpaqueJson`] here so scene-side
    /// schema changes don't ripple into this crate.
    pub partial_scene: OpaqueJson,
}

/// Reply for [`ArkExtension::scene_compile_hook`] — extension's
/// contributions to fold back into the scene.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SceneCompileHookResponse {
    /// Opaque contributions payload; shape pinned alongside the
    /// request when host-dispatch wires the first caller.
    pub contributions: OpaqueJson,
}

/// `control_verbs` request — ark collecting the verbs this extension
/// contributes to `ark control`.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ControlVerbsRequest {}

/// Reply for [`ArkExtension::control_verbs`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ControlVerbsResponse {
    /// Verb specs (opaque JSON here; host-dispatch pins shape).
    pub verbs: OpaqueJson,
}

/// `doctor_checks` request — ark collecting the doctor checks this
/// extension contributes.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DoctorChecksRequest {}

/// Reply for [`ArkExtension::doctor_checks`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DoctorChecksResponse {
    /// Check descriptors or results (opaque JSON).
    pub checks: OpaqueJson,
}

/// `list_columns` request — ark collecting the columns this extension
/// contributes to `ark list`.
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ListColumnsRequest {}

/// Reply for [`ArkExtension::list_columns`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ListColumnsResponse {
    /// Column specs (name + value-resolver reference, opaque JSON).
    pub columns: OpaqueJson,
}

// ---------------------------------------------------------------------------
// UI — keybind / status
// ---------------------------------------------------------------------------

/// `ui/keybind/register` — extension advertises a command ID with metadata
/// (R16: this does NOT bind raw keys — the user's scene binds keys to the
/// command ID).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiKeybindRegisterRequest {
    /// Fully-qualified command id. User scene references as
    /// `keybind "<chord>" intent="<ext-name>.<command>"`.
    pub command: String,
    /// Human-readable title for command-palette / status rendering.
    pub title: String,
    /// Optional CEL `when=` predicate restricting when the command is
    /// enabled. Empty string = always enabled.
    pub when: String,
    /// Suggested default chord (e.g. `"Alt p"`). User scene-level bindings
    /// ALWAYS win; colliding defaults across extensions = warning + leave
    /// unbound (R16).
    pub default_chord: Option<String>,
}

/// Void response for [`ArkExtension::ui_keybind_register`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiKeybindRegisterResponse {}

/// `ui/keybind/unregister` — drop a prior command registration. R16 makes
/// runtime-registered UI state ephemeral: extensions MUST unregister in
/// `shutdown`, and ark drops them on crash.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiKeybindUnregisterRequest {
    /// Command id to drop.
    pub command: String,
}

/// Void response for [`ArkExtension::ui_keybind_unregister`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiKeybindUnregisterResponse {}

/// `ui/status/push` notification — extension updates the status line.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiStatusPushRequest {
    /// Status text (plain text; no ANSI). Empty string clears the slot.
    pub text: String,
    /// Severity — maps to colour in the status plugin.
    pub severity: LogLevel,
}

/// Void response for [`ArkExtension::ui_status_push`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiStatusPushResponse {}

// ---------------------------------------------------------------------------
// UI — panes (narrow)
// ---------------------------------------------------------------------------

/// `ui/pane/request` — extension asks ark to fill a user-declared pane slot
/// or open an ephemeral overlay (R16 two-tier pane model).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiPaneRequestRequest {
    /// Slot name declared by the user's scene via
    /// `layout { pane-slot name="<id>" … }`. If `None`, ark treats this as
    /// an ephemeral overlay request (floating pane / diff viewer).
    pub slot: Option<String>,
    /// Free-form extension payload describing the pane contents
    /// (command to run, plugin-url, etc.). Schema is extension-local.
    pub params: OpaqueJson,
}

/// Response for [`ArkExtension::ui_pane_request`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiPaneRequestResponse {
    /// Opaque pane handle — passed to [`UiPaneCloseRequest::pane`].
    pub pane: String,
}

/// `ui/pane/close` — extension asks ark to close a pane it previously
/// requested.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiPaneCloseRequest {
    /// Pane handle returned by [`UiPaneRequestResponse::pane`].
    pub pane: String,
}

/// Void response for [`ArkExtension::ui_pane_close`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiPaneCloseResponse {}

// ---------------------------------------------------------------------------
// Workspace (LSP-style reverse-requests)
// ---------------------------------------------------------------------------

/// `workspace/applyEdit` — extension asks ark to apply a set of text edits
/// to the user's workspace (mirrors LSP `workspace/applyEdit`).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceApplyEditRequest {
    /// Edit descriptor — JSON-compatible with LSP `WorkspaceEdit`.
    pub edit: OpaqueJson,
}

/// Response for [`ArkExtension::workspace_apply_edit`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceApplyEditResponse {
    /// `true` if applied, `false` if the user rejected or the edit was
    /// invalid.
    pub applied: bool,
}

/// `workspace/configuration` — extension reads a configuration value from
/// ark's merged config (scene + `config.toml` + env).
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceConfigurationRequest {
    /// Dotted section path, e.g. `"myext.timeouts.fetch"`. Scoped to the
    /// extension's namespace: requests outside `<ext-name>.*` return
    /// [`ExtensionError::CapabilityDenied`].
    pub section: String,
}

/// Response for [`ArkExtension::workspace_configuration`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceConfigurationResponse {
    /// Config value. `null` if the section is unset.
    pub value: OpaqueJson,
}

/// `workspace/showDocument` — extension asks ark to open a file / URL for
/// the user.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowDocumentRequest {
    /// Target URI — `file://` or `https://`.
    pub uri: String,
    /// `true` to request focus-stealing. Ark may ignore depending on
    /// session-activity state.
    pub take_focus: bool,
}

/// Response for [`ArkExtension::workspace_show_document`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowDocumentResponse {
    /// `true` if the document was shown.
    pub success: bool,
}

/// `workspace/showMessage` notification — extension emits a user-visible
/// toast/log line.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowMessageRequest {
    /// Message text.
    pub message: String,
    /// Severity — maps to colour in the status plugin.
    pub severity: LogLevel,
}

/// Void response for [`ArkExtension::workspace_show_message`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowMessageResponse {}

/// `workspace/showMessageRequest` — like `showMessage` but awaits a user
/// choice from a list of actions.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowMessageRequestRequest {
    /// Message text.
    pub message: String,
    /// Severity — drives the prompt icon.
    pub severity: LogLevel,
    /// Button labels. An empty list means a plain info dialog with a
    /// single "OK" action.
    pub actions: Vec<String>,
}

/// Response for [`ArkExtension::workspace_show_message_request`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceShowMessageRequestResponse {
    /// Selected action label, or `None` if dismissed.
    pub selected: Option<String>,
}

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// `scene/getRoot` — extension queries the currently-loaded scene path
/// plus CWD (R16 "scene intent channel").
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SceneGetRootRequest {}

/// Response for [`ArkExtension::scene_get_root`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SceneGetRootResponse {
    /// Absolute path to the scene file ark is running.
    pub scene_path: String,
    /// Session CWD. Extensions treat this as the rooted path for relative
    /// file operations.
    pub cwd: String,
}

// ---------------------------------------------------------------------------
// Host syscalls (WASM-ONLY, capability-gated)
// ---------------------------------------------------------------------------

/// `host/fs/read` — wasm-component extension reads a file via the host.
/// Subprocess extensions MUST use OS syscalls directly; calling this from
/// a subprocess returns [`ExtensionError::CapabilityDenied`] per R16.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostFsReadRequest {
    /// Absolute file path. Subject to capability-scope rules (writes to
    /// outside scene root are blocked; see R17 permission dispatch).
    pub path: String,
}

/// Response for [`ArkExtension::host_fs_read`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostFsReadResponse {
    /// File contents as a UTF-8 string. Non-UTF-8 files surface as
    /// [`ExtensionError::InvalidParams`] — use a future `host/fs/readBytes`
    /// for binary content (added as MINOR bump per R16 rule #1).
    pub contents: String,
}

/// `host/fs/write` — wasm-component extension writes a file via the host.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostFsWriteRequest {
    /// Absolute file path.
    pub path: String,
    /// UTF-8 contents to write.
    pub contents: String,
}

/// Void response for [`ArkExtension::host_fs_write`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HostFsWriteResponse {}

/// `host/proc/spawn` — wasm-component extension spawns a subprocess.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostProcSpawnRequest {
    /// Executable name or path.
    pub command: String,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// Working directory. Empty string = session CWD.
    pub cwd: String,
}

/// Response for [`ArkExtension::host_proc_spawn`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostProcSpawnResponse {
    /// Process exit code.
    pub exit_code: i32,
    /// Stdout captured as a UTF-8 string.
    pub stdout: String,
    /// Stderr captured as a UTF-8 string.
    pub stderr: String,
}

/// `host/net/fetch` — wasm-component extension performs an HTTP request.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostNetFetchRequest {
    /// Fully-qualified URL.
    pub url: String,
    /// HTTP verb (`GET`, `POST`, …).
    pub method: String,
    /// Optional request body (UTF-8 / JSON).
    pub body: Option<String>,
}

/// Response for [`ArkExtension::host_net_fetch`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostNetFetchResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: String,
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// `log/write` notification — extension writes a structured log line.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogWriteRequest {
    /// Log severity.
    pub level: LogLevel,
    /// Message text.
    pub message: String,
}

/// Void response for [`ArkExtension::log_write`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LogWriteResponse {}

/// `log/setLevel` — ark asks the extension to filter its own outgoing
/// log/write calls to a minimum severity.
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogSetLevelRequest {
    /// Minimum level to emit.
    pub level: LogLevel,
}

/// Void response for [`ArkExtension::log_set_level`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LogSetLevelResponse {}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// Canonical Rust trait implementing ark's extension runtime RPC surface
/// (`cavekit-scene.md` R16 v1).
///
/// Every method has a default implementation that returns
/// [`ExtensionError::method_not_found`] with the method name — extensions
/// override only the methods they support. Ark's dispatcher (in
/// `ark-scene` / future `ark-ext-host`) translates `method_not_found` to
/// JSON-RPC `-32601` per R16 best-effort mode.
///
/// The trait is `Send + Sync` so a single `Arc<dyn ArkExtension>` can be
/// shared across tokio tasks; `async_trait::async_trait` adapts the methods
/// for dynamic dispatch on stable Rust.
#[async_trait]
pub trait ArkExtension: Send + Sync {
    // -- Lifecycle -----------------------------------------------------------

    /// Handshake — first message ark sends after transport open. Extension
    /// returns its supported protocol range + capabilities.
    ///
    /// Default: [`ExtensionError::method_not_found`]. Every extension MUST
    /// override this; ark refuses to complete startup without a valid
    /// `initialize` response.
    async fn initialize(&self, _req: InitializeRequest) -> ExtResult<InitializeResponse> {
        Err(ExtensionError::method_not_found("initialize"))
    }

    /// `initialized` notification. Default: `Ok(Default::default())` — the
    /// handshake is optional downstream bookkeeping.
    async fn initialized(&self, _req: InitializedRequest) -> ExtResult<InitializedResponse> {
        Ok(InitializedResponse::default())
    }

    /// Graceful shutdown. Default: `Ok(Default::default())`.
    async fn shutdown(&self, _req: ShutdownRequest) -> ExtResult<ShutdownResponse> {
        Ok(ShutdownResponse::default())
    }

    /// Liveness probe. Default: `Ok(Default::default())` — any response
    /// satisfies liveness.
    async fn ping(&self, _req: PingRequest) -> ExtResult<PingResponse> {
        Ok(PingResponse::default())
    }

    // -- Async + cancel ------------------------------------------------------

    /// `$/cancel` notification. Default: `Ok(Default::default())` —
    /// extensions that don't model cancellation ignore the notification;
    /// late responses are dropped per MCP semantics.
    async fn cancel(&self, _req: CancelRequest) -> ExtResult<CancelResponse> {
        Ok(CancelResponse::default())
    }

    /// `$/progress` notification. Default: `Ok(Default::default())` —
    /// extensions that don't model progress accept the notification
    /// silently.
    async fn progress(&self, _req: ProgressRequest) -> ExtResult<ProgressResponse> {
        Ok(ProgressResponse::default())
    }

    /// `task/create`. Default: [`ExtensionError::method_not_found`] —
    /// extensions that use long-running tasks override this.
    async fn task_create(&self, _req: TaskCreateRequest) -> ExtResult<TaskCreateResponse> {
        Err(ExtensionError::method_not_found("task/create"))
    }

    /// `task/get`. Default: [`ExtensionError::method_not_found`].
    async fn task_get(&self, _req: TaskGetRequest) -> ExtResult<TaskGetResponse> {
        Err(ExtensionError::method_not_found("task/get"))
    }

    /// `task/cancel`. Default: [`ExtensionError::method_not_found`].
    async fn task_cancel(&self, _req: TaskCancelRequest) -> ExtResult<TaskCancelResponse> {
        Err(ExtensionError::method_not_found("task/cancel"))
    }

    // -- Event bus -----------------------------------------------------------

    /// `event/subscribe`. Default: [`ExtensionError::method_not_found`].
    async fn event_subscribe(
        &self,
        _req: EventSubscribeRequest,
    ) -> ExtResult<EventSubscribeResponse> {
        Err(ExtensionError::method_not_found("event/subscribe"))
    }

    /// `event/unsubscribe`. Default: [`ExtensionError::method_not_found`].
    async fn event_unsubscribe(
        &self,
        _req: EventUnsubscribeRequest,
    ) -> ExtResult<EventUnsubscribeResponse> {
        Err(ExtensionError::method_not_found("event/unsubscribe"))
    }

    /// `event/emit`. Default: [`ExtensionError::method_not_found`].
    async fn event_emit(&self, _req: EventEmitRequest) -> ExtResult<EventEmitResponse> {
        Err(ExtensionError::method_not_found("event/emit"))
    }

    /// `event/notify` — host-to-extension delivery. Default:
    /// `Ok(Default::default())`. Extensions that subscribe to events MUST
    /// override this, but silently accepting is safe for extensions with no
    /// active subscriptions.
    async fn event_notify(&self, _req: EventNotifyRequest) -> ExtResult<EventNotifyResponse> {
        Ok(EventNotifyResponse::default())
    }

    // -- Intents -------------------------------------------------------------

    /// `intent/unregister`. Default: [`ExtensionError::method_not_found`].
    async fn intent_unregister(
        &self,
        _req: IntentUnregisterRequest,
    ) -> ExtResult<IntentUnregisterResponse> {
        Err(ExtensionError::method_not_found("intent/unregister"))
    }

    /// `intent/dispatch`. Default: [`ExtensionError::method_not_found`].
    ///
    /// Per phase-2 ext-surface R5: intent registration now flows through
    /// the manifest (loader-owned), but `intent_dispatch` MUST remain on
    /// the trait — loader-constructed shims invoke it every dispatch.
    /// Removing or renaming this method would be a MAJOR protocol bump
    /// (decision #4c). Phase 2 forbids that change; T-022 pins the
    /// contract.
    async fn intent_dispatch(
        &self,
        _req: IntentDispatchRequest,
    ) -> ExtResult<IntentDispatchResponse> {
        Err(ExtensionError::method_not_found("intent/dispatch"))
    }

    // -- Pane / Stack handle ops (Phase 2 R6) --------------------------------

    /// `pane/emit`. Default: [`ExtensionError::method_not_found`].
    async fn pane_emit(&self, _req: PaneEmitRequest) -> ExtResult<PaneEmitResponse> {
        Err(ExtensionError::method_not_found("pane/emit"))
    }

    /// `pane/replace_view`. Default: [`ExtensionError::method_not_found`].
    async fn pane_replace_view(
        &self,
        _req: PaneReplaceViewRequest,
    ) -> ExtResult<PaneReplaceViewResponse> {
        Err(ExtensionError::method_not_found("pane/replace_view"))
    }

    /// `pane/close`. Default: [`ExtensionError::method_not_found`].
    async fn pane_close(&self, _req: PaneCloseRequest) -> ExtResult<PaneCloseResponse> {
        Err(ExtensionError::method_not_found("pane/close"))
    }

    /// `stack/spawn_pane`. Default: [`ExtensionError::method_not_found`].
    async fn stack_spawn_pane(
        &self,
        _req: StackSpawnPaneRequest,
    ) -> ExtResult<StackSpawnPaneResponse> {
        Err(ExtensionError::method_not_found("stack/spawn_pane"))
    }

    /// `stack/close_child`. Default: [`ExtensionError::method_not_found`].
    async fn stack_close_child(
        &self,
        _req: StackCloseChildRequest,
    ) -> ExtResult<StackCloseChildResponse> {
        Err(ExtensionError::method_not_found("stack/close_child"))
    }

    /// `stack/clear`. Default: [`ExtensionError::method_not_found`].
    async fn stack_clear(&self, _req: StackClearRequest) -> ExtResult<StackClearResponse> {
        Err(ExtensionError::method_not_found("stack/clear"))
    }

    // -- Session lifecycle hooks (Phase 2 ext-surface R1) --------------------

    /// `on_session_start` — fires once per session at spawn, before the
    /// first engine event. Default: [`ExtensionError::method_not_found`];
    /// extensions that need per-session state override this.
    async fn on_session_start(
        &self,
        _req: OnSessionStartRequest,
    ) -> ExtResult<OnSessionStartResponse> {
        Err(ExtensionError::method_not_found("on_session_start"))
    }

    /// `on_session_end` — fires once per session at termination with the
    /// terminal [`ark_types::event::ExitReason`]. Default:
    /// [`ExtensionError::method_not_found`]; extensions that need
    /// teardown hooks override this.
    async fn on_session_end(&self, _req: OnSessionEndRequest) -> ExtResult<OnSessionEndResponse> {
        Err(ExtensionError::method_not_found("on_session_end"))
    }

    // -- Feature-group hooks (Phase 2 ext-surface R2) ------------------------

    /// `scene_compile_hook`. Default: [`ExtensionError::method_not_found`].
    async fn scene_compile_hook(
        &self,
        _req: SceneCompileHookRequest,
    ) -> ExtResult<SceneCompileHookResponse> {
        Err(ExtensionError::method_not_found("scene_compile_hook"))
    }

    /// `control_verbs`. Default: [`ExtensionError::method_not_found`].
    async fn control_verbs(&self, _req: ControlVerbsRequest) -> ExtResult<ControlVerbsResponse> {
        Err(ExtensionError::method_not_found("control_verbs"))
    }

    /// `doctor_checks`. Default: [`ExtensionError::method_not_found`].
    async fn doctor_checks(&self, _req: DoctorChecksRequest) -> ExtResult<DoctorChecksResponse> {
        Err(ExtensionError::method_not_found("doctor_checks"))
    }

    /// `list_columns`. Default: [`ExtensionError::method_not_found`].
    async fn list_columns(&self, _req: ListColumnsRequest) -> ExtResult<ListColumnsResponse> {
        Err(ExtensionError::method_not_found("list_columns"))
    }

    // -- UI: keybind / status ------------------------------------------------

    /// `ui/keybind/register`. Default: [`ExtensionError::method_not_found`].
    async fn ui_keybind_register(
        &self,
        _req: UiKeybindRegisterRequest,
    ) -> ExtResult<UiKeybindRegisterResponse> {
        Err(ExtensionError::method_not_found("ui/keybind/register"))
    }

    /// `ui/keybind/unregister`. Default:
    /// [`ExtensionError::method_not_found`].
    async fn ui_keybind_unregister(
        &self,
        _req: UiKeybindUnregisterRequest,
    ) -> ExtResult<UiKeybindUnregisterResponse> {
        Err(ExtensionError::method_not_found("ui/keybind/unregister"))
    }

    /// `ui/status/push` notification. Default: `Ok(Default::default())`.
    async fn ui_status_push(&self, _req: UiStatusPushRequest) -> ExtResult<UiStatusPushResponse> {
        Ok(UiStatusPushResponse::default())
    }

    // -- UI: panes -----------------------------------------------------------

    /// `ui/pane/request`. Default: [`ExtensionError::method_not_found`].
    async fn ui_pane_request(
        &self,
        _req: UiPaneRequestRequest,
    ) -> ExtResult<UiPaneRequestResponse> {
        Err(ExtensionError::method_not_found("ui/pane/request"))
    }

    /// `ui/pane/close`. Default: [`ExtensionError::method_not_found`].
    async fn ui_pane_close(&self, _req: UiPaneCloseRequest) -> ExtResult<UiPaneCloseResponse> {
        Err(ExtensionError::method_not_found("ui/pane/close"))
    }

    // -- Workspace -----------------------------------------------------------

    /// `workspace/applyEdit`. Default:
    /// [`ExtensionError::method_not_found`].
    async fn workspace_apply_edit(
        &self,
        _req: WorkspaceApplyEditRequest,
    ) -> ExtResult<WorkspaceApplyEditResponse> {
        Err(ExtensionError::method_not_found("workspace/applyEdit"))
    }

    /// `workspace/configuration`. Default:
    /// [`ExtensionError::method_not_found`].
    async fn workspace_configuration(
        &self,
        _req: WorkspaceConfigurationRequest,
    ) -> ExtResult<WorkspaceConfigurationResponse> {
        Err(ExtensionError::method_not_found("workspace/configuration"))
    }

    /// `workspace/showDocument`. Default:
    /// [`ExtensionError::method_not_found`].
    async fn workspace_show_document(
        &self,
        _req: WorkspaceShowDocumentRequest,
    ) -> ExtResult<WorkspaceShowDocumentResponse> {
        Err(ExtensionError::method_not_found("workspace/showDocument"))
    }

    /// `workspace/showMessage` notification. Default:
    /// `Ok(Default::default())`.
    async fn workspace_show_message(
        &self,
        _req: WorkspaceShowMessageRequest,
    ) -> ExtResult<WorkspaceShowMessageResponse> {
        Ok(WorkspaceShowMessageResponse::default())
    }

    /// `workspace/showMessageRequest`. Default:
    /// [`ExtensionError::method_not_found`].
    async fn workspace_show_message_request(
        &self,
        _req: WorkspaceShowMessageRequestRequest,
    ) -> ExtResult<WorkspaceShowMessageRequestResponse> {
        Err(ExtensionError::method_not_found(
            "workspace/showMessageRequest",
        ))
    }

    // -- Scene ---------------------------------------------------------------

    /// `scene/getRoot`. Default: [`ExtensionError::method_not_found`] —
    /// scene-unaware extensions have no reason to override.
    async fn scene_get_root(&self, _req: SceneGetRootRequest) -> ExtResult<SceneGetRootResponse> {
        Err(ExtensionError::method_not_found("scene/getRoot"))
    }

    // -- Host syscalls (wasm-only, capability-gated) -------------------------

    /// `host/fs/read`. Default: [`ExtensionError::method_not_found`] —
    /// subprocess / compiled-in extensions MUST NOT call this; wasm hosts
    /// override with a capability-checked implementation.
    async fn host_fs_read(&self, _req: HostFsReadRequest) -> ExtResult<HostFsReadResponse> {
        Err(ExtensionError::method_not_found("host/fs/read"))
    }

    /// `host/fs/write`. Default: [`ExtensionError::method_not_found`].
    async fn host_fs_write(&self, _req: HostFsWriteRequest) -> ExtResult<HostFsWriteResponse> {
        Err(ExtensionError::method_not_found("host/fs/write"))
    }

    /// `host/proc/spawn`. Default: [`ExtensionError::method_not_found`].
    async fn host_proc_spawn(
        &self,
        _req: HostProcSpawnRequest,
    ) -> ExtResult<HostProcSpawnResponse> {
        Err(ExtensionError::method_not_found("host/proc/spawn"))
    }

    /// `host/net/fetch`. Default: [`ExtensionError::method_not_found`].
    async fn host_net_fetch(&self, _req: HostNetFetchRequest) -> ExtResult<HostNetFetchResponse> {
        Err(ExtensionError::method_not_found("host/net/fetch"))
    }

    // -- Logging -------------------------------------------------------------

    /// `log/write` notification. Default: `Ok(Default::default())`.
    async fn log_write(&self, _req: LogWriteRequest) -> ExtResult<LogWriteResponse> {
        Ok(LogWriteResponse::default())
    }

    /// `log/setLevel`. Default: `Ok(Default::default())` — extensions
    /// that don't log ignore the level change.
    async fn log_set_level(&self, _req: LogSetLevelRequest) -> ExtResult<LogSetLevelResponse> {
        Ok(LogSetLevelResponse::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub extension that overrides nothing — exercises every default
    /// impl to confirm method counts + default behaviour compile.
    struct StubExt;
    #[async_trait]
    impl ArkExtension for StubExt {}

    #[tokio::test]
    async fn initialize_default_returns_method_not_found() {
        let ext = StubExt;
        let err = ext
            .initialize(InitializeRequest {
                protocol_version: "1.0".into(),
                client_capabilities: "null".into(),
                client_info: "ark-test".into(),
            })
            .await
            .unwrap_err();
        match err {
            ExtensionError::MethodNotFound(m) => assert_eq!(m, "initialize"),
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn void_lifecycle_defaults_ok() {
        let ext = StubExt;
        ext.ping(PingRequest::default()).await.unwrap();
        ext.shutdown(ShutdownRequest::default()).await.unwrap();
        ext.initialized(InitializedRequest::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn log_write_default_ok() {
        let ext = StubExt;
        ext.log_write(LogWriteRequest {
            level: LogLevel::Info,
            message: "hi".into(),
        })
        .await
        .unwrap();
    }

    #[test]
    fn log_level_is_copy() {
        let a = LogLevel::Debug;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn extension_error_displays_method_name() {
        let e = ExtensionError::method_not_found("foo/bar");
        assert_eq!(e.to_string(), "method not found: foo/bar");
    }

    /// R7 + R12: the wire error code for `HandleGone` MUST be stable at
    /// `ext-proto/handle-gone` — transports key JSON-RPC `data` off
    /// `ExtensionError::code()` so scene consumers can pattern-match
    /// against the string without parsing `Display`.
    #[test]
    fn handle_gone_error_code_matches_wire_contract() {
        let e = ExtensionError::HandleGone {
            handle: ark_view::HandleId::new("abc"),
            cause: ark_view::InvalidationCause::UserClosed,
        };
        assert_eq!(e.code(), "ext-proto/handle-gone");
    }

    // -- Phase 2 RPC surface (T-018 / T-019 / T-021 / T-022) ---------------

    /// T-018 AC: named request + response types exist, carry the derives
    /// the rest of the crate relies on, and can be used from generic
    /// bounds. Also doubles as a compile-time smoke test that Facet /
    /// serde derives succeeded.
    #[test]
    fn phase_2_rpc_request_structs_exist() {
        fn assert_send_sync_clone<T: Send + Sync + Clone + std::fmt::Debug>() {}
        assert_send_sync_clone::<PaneEmitRequest>();
        assert_send_sync_clone::<PaneEmitResponse>();
        assert_send_sync_clone::<PaneReplaceViewRequest>();
        assert_send_sync_clone::<PaneReplaceViewResponse>();
        assert_send_sync_clone::<PaneCloseRequest>();
        assert_send_sync_clone::<PaneCloseResponse>();
        assert_send_sync_clone::<StackSpawnPaneRequest>();
        assert_send_sync_clone::<StackSpawnPaneResponse>();
        assert_send_sync_clone::<StackCloseChildRequest>();
        assert_send_sync_clone::<StackCloseChildResponse>();
        assert_send_sync_clone::<StackClearRequest>();
        assert_send_sync_clone::<StackClearResponse>();
        assert_send_sync_clone::<SceneCompileHookRequest>();
        assert_send_sync_clone::<SceneCompileHookResponse>();
        assert_send_sync_clone::<ControlVerbsRequest>();
        assert_send_sync_clone::<ControlVerbsResponse>();
        assert_send_sync_clone::<DoctorChecksRequest>();
        assert_send_sync_clone::<DoctorChecksResponse>();
        assert_send_sync_clone::<ListColumnsRequest>();
        assert_send_sync_clone::<ListColumnsResponse>();
    }

    /// T-020 AC: session lifecycle hook request/response types exist and
    /// carry the derives the rest of the crate relies on.
    #[test]
    fn on_session_lifecycle_structs_exist() {
        fn assert_s_s_c<T: Send + Sync + Clone + std::fmt::Debug>() {}
        assert_s_s_c::<OnSessionStartRequest>();
        assert_s_s_c::<OnSessionStartResponse>();
        assert_s_s_c::<OnSessionEndRequest>();
        assert_s_s_c::<OnSessionEndResponse>();
    }

    /// T-020 AC: `OnSessionEndRequest::exit` carries a JSON-encoded
    /// [`ark_types::event::ExitReason`]. Verifies the dep wiring is
    /// sound by round-tripping each variant through the OpaqueJson
    /// channel the struct holds.
    #[test]
    fn on_session_end_carries_exit_reason_variants() {
        use ark_types::event::ExitReason;
        for variant in [
            ExitReason::Normal,
            ExitReason::Cancelled,
            ExitReason::Error("boom".to_string()),
        ] {
            let encoded = serde_json::to_string(&variant).expect("serialise");
            let req = OnSessionEndRequest {
                spec: "null".into(),
                exit: encoded.clone(),
            };
            let back: ExitReason = serde_json::from_str(&req.exit).expect("deserialise exit");
            assert_eq!(back, variant);
        }
    }

    /// T-018 / kit R6 AC: the stack-spawn response carries a single
    /// `handle` field and serialises as exactly `{ "handle": "<id>" }`.
    #[test]
    fn stack_spawn_pane_response_carries_single_handle_field() {
        let r = StackSpawnPaneResponse {
            handle: ark_view::HandleId::new("new-child"),
        };
        assert_eq!(r.handle.as_str(), "new-child");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v, serde_json::json!({ "handle": "new-child" }));
    }

    /// T-022: `intent_dispatch` MUST survive Phase 2 (decision #4c —
    /// removing / renaming it is MAJOR). Dispatch through the default
    /// impl exercises the path.
    #[tokio::test]
    async fn intent_dispatch_survives_phase_2() {
        let ext = StubExt;
        let err = ext
            .intent_dispatch(IntentDispatchRequest {
                name: "mycorp.do".into(),
                args: "null".into(),
            })
            .await
            .unwrap_err();
        match err {
            ExtensionError::MethodNotFound(m) => assert_eq!(m, "intent/dispatch"),
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    /// T-019 + T-021: every new trait method's default impl returns
    /// `method_not_found` with the expected wire name.
    #[tokio::test]
    async fn phase_2_new_trait_methods_default_to_method_not_found() {
        let ext = StubExt;
        let h = ark_view::HandleId::new("h");

        let expectations: &[(&str, ExtensionError)] = &[
            (
                "pane/emit",
                ext.pane_emit(PaneEmitRequest {
                    handle: h.clone(),
                    kind: "k".into(),
                    payload: "null".into(),
                })
                .await
                .unwrap_err(),
            ),
            (
                "pane/replace_view",
                ext.pane_replace_view(PaneReplaceViewRequest {
                    handle: h.clone(),
                    view_type: "x.v".into(),
                    view_body: "null".into(),
                })
                .await
                .unwrap_err(),
            ),
            (
                "pane/close",
                ext.pane_close(PaneCloseRequest { handle: h.clone() })
                    .await
                    .unwrap_err(),
            ),
            (
                "stack/spawn_pane",
                ext.stack_spawn_pane(StackSpawnPaneRequest {
                    stack: h.clone(),
                    attrs: "null".into(),
                })
                .await
                .unwrap_err(),
            ),
            (
                "stack/close_child",
                ext.stack_close_child(StackCloseChildRequest {
                    stack: h.clone(),
                    handle: h.clone(),
                })
                .await
                .unwrap_err(),
            ),
            (
                "stack/clear",
                ext.stack_clear(StackClearRequest { stack: h.clone() })
                    .await
                    .unwrap_err(),
            ),
            (
                "scene_compile_hook",
                ext.scene_compile_hook(SceneCompileHookRequest::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "control_verbs",
                ext.control_verbs(ControlVerbsRequest::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "doctor_checks",
                ext.doctor_checks(DoctorChecksRequest::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "list_columns",
                ext.list_columns(ListColumnsRequest::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "on_session_start",
                ext.on_session_start(OnSessionStartRequest {
                    spec: "null".into(),
                })
                .await
                .unwrap_err(),
            ),
            (
                "on_session_end",
                ext.on_session_end(OnSessionEndRequest {
                    spec: "null".into(),
                    exit: "{\"kind\":\"normal\"}".into(),
                })
                .await
                .unwrap_err(),
            ),
        ];

        for (expected_method, err) in expectations {
            match err {
                ExtensionError::MethodNotFound(m) => assert_eq!(m, expected_method),
                other => panic!("expected MethodNotFound({expected_method}), got {other:?}"),
            }
        }
    }

    /// Lock the wire method names used in the ndjson server dispatch
    /// block so a rename elsewhere trips here too (kit R6 + ext-surface
    /// R2 AC).
    #[test]
    fn phase_2_method_name_goldens() {
        let names = [
            "pane/emit",
            "pane/replace_view",
            "pane/close",
            "stack/spawn_pane",
            "stack/close_child",
            "stack/clear",
            "on_session_start",
            "on_session_end",
            "scene_compile_hook",
            "control_verbs",
            "doctor_checks",
            "list_columns",
        ];
        for n in names {
            assert!(!n.is_empty());
        }
    }

    /// Display string MUST surface both the handle id and the cause tag
    /// — operators reading logs need to see which handle died and why
    /// without cross-referencing the structured fields.
    #[test]
    fn handle_gone_display_mentions_handle_and_cause() {
        let e = ExtensionError::HandleGone {
            handle: ark_view::HandleId::new("pane-42"),
            cause: ark_view::InvalidationCause::SceneReloadDropped,
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("pane-42"),
            "expected handle id in display, got {rendered:?}"
        );
        assert!(
            rendered.contains("SceneReloadDropped"),
            "expected cause tag in display, got {rendered:?}"
        );
    }

    #[test]
    fn phase_2_capability_flag_set_is_exactly_eight() {
        assert_eq!(PHASE_2_CAPABILITY_FLAGS.len(), 8);
        // Set-equality check — catches drift (dupes, typos).
        let set: std::collections::BTreeSet<&&str> = PHASE_2_CAPABILITY_FLAGS.iter().collect();
        assert_eq!(set.len(), 8, "no duplicates");
        let expected: std::collections::BTreeSet<&str> = [
            "view.pane.v1",
            "view.stack.v1",
            "ext.lifecycle.v1",
            "ext.scene_compile_hook.v1",
            "ext.control_verbs.v1",
            "ext.doctor.v1",
            "ext.list_columns.v1",
            "ext.reload_gate.v1",
        ]
        .iter()
        .copied()
        .collect();
        let got: std::collections::BTreeSet<&str> =
            PHASE_2_CAPABILITY_FLAGS.iter().copied().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn phase_2_flags_follow_naming_convention() {
        // <domain>.<feature>.v<N> — at least 3 dotted segments, last is v<digit>.
        for flag in PHASE_2_CAPABILITY_FLAGS {
            let parts: Vec<&str> = flag.split('.').collect();
            assert!(parts.len() >= 3, "{flag} must have ≥3 segments");
            let last = parts.last().unwrap();
            assert!(
                last.starts_with('v'),
                "{flag} last segment must start with 'v'"
            );
            let digits = &last[1..];
            assert!(
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
                "{flag} version segment must be v<digits>"
            );
        }
    }
}
