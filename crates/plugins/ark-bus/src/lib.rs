//! `ark-bus` — headless zellij wasm plugin bridging zellij-internal events
//! to the ark supervisor control socket via the hidden-command-pane
//! pattern.
//!
//! # Role
//!
//! Zellij keybinds (per cavekit-scene.md R5) dispatch user intents via the
//! zellij plugin protocol; the ark supervisor owns the control socket. This
//! plugin is the headless bridge that sits between the two — it consumes
//! zellij events and pipe messages inside the zellij process and forwards
//! them to the supervisor over IPC. See `context/kits/cavekit-scene.md` R5
//! (keybinds → ark-bus intent dispatch), R6 (plugin lifecycle), and the
//! runtime section in `context/kits/cavekit-architecture.md`.
//!
//! # Why hidden command panes (and not direct sockets)?
//!
//! `zellij-tile`'s wasi sandbox does not expose a unix-socket API
//! (`std::os::unix::net` is unavailable in `wasm32-wasip1`). The plugin
//! instead spawns a hidden command pane running `ark-hook intent --json
//! '<payload>'` (T-6.2). The hook binary is on the host filesystem, has
//! socket access, and reaches the supervisor in <50ms — well inside the
//! keybind UX budget.
//!
//! See `cavekit-hook-ipc.md` R1 for the `ark-hook` subcommand surface and
//! R5 for the supervisor's matching `Intent` / `Emit` / `Permit`
//! commands.
//!
//! # Pipe endpoints
//!
//! - `ark-intent` (T-6.2): payload is the verbatim JSON document to pass
//!   to `ark-hook intent --json`. Schema: `{"name": "<op>", "args": {…}}`.
//! - `ark-rebind` (T-6.4 — pending): payload describes a `rebind_keys`
//!   plan applied via the `zellij_tile::shim::rebind_keys` host call.
//!
//! # Subscribed events
//!
//! T-6.3 will subscribe to the four pane-lifecycle events
//! (`CommandPaneOpened`, `CommandPaneExited`, `PaneClosed`,
//! `FileSystemUpdate`) and forward each as a `UserEvent` via the
//! supervisor's `Emit` command.
//!
//! # Target gating
//!
//! Mirrors `ark-plugin-status` and `ark-plugin-picker`: the `ZellijPlugin`
//! impl and `register_plugin!` expansion link against wasm-only
//! `host_run_plugin_command` symbols, so both are gated behind
//! `#[cfg(target_arch = "wasm32")]`. Host builds still compile the crate
//! (keeps `cargo check --workspace` green) but skip the wasm-only
//! registration. The pure JSON-shaping helpers used by the plugin live
//! outside the `wasm_plugin` module so host-side unit tests can exercise
//! them without a wasm runtime.

/// Registered plugin name used by supervisors when targeting this plugin
/// via `zellij pipe --name`. Declared as a constant so dispatchers and
/// (future) ingestion filters share a single source of truth.
pub const PLUGIN_NAME: &str = "ark-bus";

/// Pipe endpoint name for intent dispatch (T-6.2). Scene-compiled
/// `keybind` actions post `MessagePlugin "ark-bus" name="ark-intent"
/// payload=<json>` to this endpoint.
pub const PIPE_INTENT: &str = "ark-intent";

/// Pipe endpoint name for the rebind dispatcher (T-6.4 — placeholder
/// until the handler lands).
pub const PIPE_REBIND: &str = "ark-rebind";

/// Headless bridge between zellij-internal events and the ark supervisor
/// control socket.
///
/// State today is intentionally empty — the plugin neither caches
/// pending intents nor pre-resolves the supervisor socket path (the
/// `ark-hook` binary handles socket resolution per R4 path scheme).
/// Future bookkeeping (e.g. per-intent telemetry, batching) goes here.
#[derive(Debug, Default)]
pub struct ArkBus;

/// Validate a payload destined for the `ark-intent` endpoint and emit a
/// canonical wire form for `ark-hook intent --json <…>`.
///
/// The plugin keeps validation **loose**: anything that parses as a
/// JSON object with a `name` string is accepted; further validation
/// (op-name lookup, args shape) lives in the supervisor side per R5.
/// This matches the R12 "compile-time strict, runtime tolerant"
/// philosophy — the scene compiler already filters intents at compile
/// time, so a runtime payload here is by construction valid.
///
/// Errors:
/// * `BadJson` — payload doesn't parse as JSON.
/// * `MissingName` — JSON object lacks a `name` string field.
/// * `NotObject` — top-level JSON value isn't an object.
pub fn validate_intent_payload(payload: &str) -> Result<String, IntentPayloadError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| IntentPayloadError::BadJson(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or(IntentPayloadError::NotObject)?;
    let _name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or(IntentPayloadError::MissingName)?;
    // Re-serialize to a single-line form so the spawned `ark-hook` sees
    // a deterministic shape (helps with stderr log-line attribution).
    Ok(value.to_string())
}

/// Errors returned by [`validate_intent_payload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentPayloadError {
    /// Payload was not valid JSON. Inner string is the parser's
    /// rendered output.
    BadJson(String),
    /// Payload was JSON but not an object at the top level.
    NotObject,
    /// JSON object did not carry a `name` string field.
    MissingName,
}

impl std::fmt::Display for IntentPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntentPayloadError::BadJson(msg) => write!(f, "ark-intent payload is not valid JSON: {msg}"),
            IntentPayloadError::NotObject => write!(
                f,
                "ark-intent payload must be a JSON object with at least a `name` field"
            ),
            IntentPayloadError::MissingName => write!(
                f,
                "ark-intent payload missing required `name` string field"
            ),
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::{ArkBus, IntentPayloadError, PIPE_INTENT, PLUGIN_NAME, validate_intent_payload};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use zellij_tile::prelude::*;

    impl ZellijPlugin for ArkBus {
        /// Lifecycle entry. ark-bus needs `RunCommands` permission to
        /// spawn the hidden `ark-hook` command pane; later (T-6.3) we
        /// will also subscribe to pane-lifecycle events here.
        fn load(&mut self, _configuration: BTreeMap<String, String>) {
            eprintln!("{PLUGIN_NAME}: load");
            // R6 of cavekit-scene + zellij-tile docs: every host-call
            // surface (pipe, command pane, rebind, subscribe) needs an
            // explicit permission grant from the user on first run.
            // ark-bus needs:
            //   * `ReadCliPipes` — to receive `MessagePlugin` payloads
            //     posted by scene-compiled keybinds.
            //   * `RunCommands` — to spawn the hidden `ark-hook` panes
            //     that bridge into the supervisor control socket.
            request_permission(&[PermissionType::ReadCliPipes, PermissionType::RunCommands]);
        }

        /// Lifecycle breadcrumb — T-6.3 will route subscribed events
        /// through here. Today this is intentionally a no-op.
        fn update(&mut self, _event: Event) -> bool {
            false
        }

        /// Dispatch a pipe message. T-6.2 handles the `ark-intent`
        /// endpoint by spawning a hidden command pane running
        /// `ark-hook intent --json '<payload>'`.
        fn pipe(&mut self, msg: PipeMessage) -> bool {
            // Only handle our endpoints; everything else is silently
            // ignored (other plugins may share the bus).
            match msg.name.as_str() {
                PIPE_INTENT => dispatch_intent(&msg),
                _ => {
                    // Foreign endpoint — not for us.
                    false
                }
            }
        }

        /// Headless: ark-bus is hosted in a hidden pane and never paints.
        fn render(&mut self, _rows: usize, _cols: usize) {}
    }

    /// Spawn `ark-hook intent --json '<payload>'` in a hidden command
    /// pane, returning `false` (no plugin re-render needed).
    fn dispatch_intent(msg: &PipeMessage) -> bool {
        let payload = match msg.payload.as_deref() {
            Some(p) => p,
            None => {
                eprintln!("{PLUGIN_NAME}: ark-intent message has no payload; ignoring");
                return false;
            }
        };
        let canonical = match validate_intent_payload(payload) {
            Ok(s) => s,
            Err(IntentPayloadError::BadJson(msg)) => {
                eprintln!("{PLUGIN_NAME}: ark-intent payload not valid JSON: {msg}");
                return false;
            }
            Err(e) => {
                eprintln!("{PLUGIN_NAME}: ark-intent payload rejected: {e}");
                return false;
            }
        };

        let cmd = CommandToRun {
            path: PathBuf::from("ark-hook"),
            args: vec!["intent".to_string(), "--json".to_string(), canonical],
            cwd: None,
        };
        // Run as a hidden background command pane — operator never
        // sees the bridge subprocess, but its stderr lands in the
        // zellij log so failed dispatches are debuggable.
        let _ = open_command_pane_background(cmd, BTreeMap::new());
        false
    }

    register_plugin!(ArkBus);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host-side smoke test — `ArkBus::default()` must instantiate cleanly
    /// without touching the wasm-only `zellij_tile` host imports.
    #[test]
    fn default_constructs_on_host() {
        let _bus = ArkBus::default();
    }

    /// Guard the registered plugin name — supervisors key `zellij pipe
    /// --name` dispatches against this string.
    #[test]
    fn plugin_name_is_stable() {
        assert_eq!(PLUGIN_NAME, "ark-bus");
        assert_eq!(PIPE_INTENT, "ark-intent");
        assert_eq!(PIPE_REBIND, "ark-rebind");
    }

    #[test]
    fn validate_intent_payload_accepts_valid_object() {
        let canonical = validate_intent_payload(
            r#"{ "name": "ark.core.open_tab", "args": { "name": "build" } }"#,
        )
        .expect("ok");
        // Re-serialised form has no spaces and is single-line.
        assert!(canonical.contains("\"name\":\"ark.core.open_tab\""));
        assert!(!canonical.contains("\n"));
    }

    #[test]
    fn validate_intent_payload_rejects_non_json() {
        let err = validate_intent_payload("not json").expect_err("must error");
        assert!(matches!(err, IntentPayloadError::BadJson(_)));
    }

    #[test]
    fn validate_intent_payload_rejects_array() {
        let err = validate_intent_payload(r#"["a","b"]"#).expect_err("must error");
        assert_eq!(err, IntentPayloadError::NotObject);
    }

    #[test]
    fn validate_intent_payload_rejects_missing_name() {
        let err = validate_intent_payload(r#"{ "args": {} }"#).expect_err("must error");
        assert_eq!(err, IntentPayloadError::MissingName);
    }

    #[test]
    fn validate_intent_payload_rejects_non_string_name() {
        let err = validate_intent_payload(r#"{ "name": 42 }"#).expect_err("must error");
        assert_eq!(err, IntentPayloadError::MissingName);
    }

    #[test]
    fn intent_payload_error_display_is_useful() {
        // Sanity: every variant renders a non-empty string for stderr.
        for e in [
            IntentPayloadError::BadJson("eof".to_string()),
            IntentPayloadError::NotObject,
            IntentPayloadError::MissingName,
        ] {
            let s = e.to_string();
            assert!(!s.is_empty());
            assert!(s.contains("ark-intent"));
        }
    }
}
