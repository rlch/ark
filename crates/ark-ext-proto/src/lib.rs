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
//! * **Intents** — [`ArkExtension::intent_register`],
//!   [`ArkExtension::intent_unregister`], [`ArkExtension::intent_dispatch`].
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

pub mod transport;

pub use transport::{ExtensionClient, InProcClient, NdjsonClient, NdjsonServer, RequestOptions};

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
}

impl ExtensionError {
    /// Construct a [`ExtensionError::MethodNotFound`] variant for the
    /// default-impl fall-through path. Every trait method's default impl
    /// calls this with its own name so ark's dispatcher can convert it to
    /// JSON-RPC `-32601`.
    pub fn method_not_found(method: impl Into<String>) -> Self {
        ExtensionError::MethodNotFound(method.into())
    }
}

/// Result alias used by every [`ArkExtension`] method.
pub type ExtResult<T> = Result<T, ExtensionError>;

// ---------------------------------------------------------------------------
// Common value types
// ---------------------------------------------------------------------------

/// Log severity for [`ArkExtension::log_write`] calls, aligned with LSP
/// `window/logMessage` message types and the `tracing` crate's `Level`.
#[derive(
    Facet,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
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
#[derive(
    Facet,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
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

/// `intent/register` — extension advertises a named intent. Namespace rule:
/// `<ext-name>.<intent>` (R10). Re-registering an intent already registered
/// by this extension replaces it; colliding with another extension's intent
/// returns [`ExtensionError::InvalidParams`].
#[derive(Facet, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntentRegisterRequest {
    /// Fully-qualified intent name.
    pub name: String,
    /// Argument schema as JSON-Schema. Ark validates `intent/dispatch`
    /// args against this schema before forwarding.
    pub args_schema: OpaqueJson,
}

/// Void response for [`ArkExtension::intent_register`].
#[derive(Facet, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IntentRegisterResponse {}

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

    /// `intent/register`. Default: [`ExtensionError::method_not_found`].
    async fn intent_register(
        &self,
        _req: IntentRegisterRequest,
    ) -> ExtResult<IntentRegisterResponse> {
        Err(ExtensionError::method_not_found("intent/register"))
    }

    /// `intent/unregister`. Default: [`ExtensionError::method_not_found`].
    async fn intent_unregister(
        &self,
        _req: IntentUnregisterRequest,
    ) -> ExtResult<IntentUnregisterResponse> {
        Err(ExtensionError::method_not_found("intent/unregister"))
    }

    /// `intent/dispatch`. Default: [`ExtensionError::method_not_found`].
    async fn intent_dispatch(
        &self,
        _req: IntentDispatchRequest,
    ) -> ExtResult<IntentDispatchResponse> {
        Err(ExtensionError::method_not_found("intent/dispatch"))
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
    async fn ui_status_push(
        &self,
        _req: UiStatusPushRequest,
    ) -> ExtResult<UiStatusPushResponse> {
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
    async fn scene_get_root(
        &self,
        _req: SceneGetRootRequest,
    ) -> ExtResult<SceneGetRootResponse> {
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
    async fn host_net_fetch(
        &self,
        _req: HostNetFetchRequest,
    ) -> ExtResult<HostNetFetchResponse> {
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
        ext.initialized(InitializedRequest::default()).await.unwrap();
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
}
