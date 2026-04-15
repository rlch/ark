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

/// Canonical UserEvent name prefix for zellij-side events forwarded
/// onto the ark event bus (T-6.3). Each subscribed zellij `Event`
/// becomes `UserEvent { name: "ark.zellij.<kind>", … }` so scene
/// reactions can listen via selectors of the form
/// `UserEvent:ark.zellij.<kind>`.
pub const FORWARDED_EVENT_PREFIX: &str = "ark.zellij.";

/// Canonical attribution tag used by every event ark-bus emits onto
/// the supervisor's event bus (T-6.3). Per `cavekit-scene.md` R4 the
/// `source` field on a `UserEvent` MUST be one of
/// `core | scene | ext:<n> | plugin:<n> | hook:<n> | agent` — `ext:`
/// is correct here because ark-bus is a wasm extension, not core code.
pub const FORWARDED_EVENT_SOURCE: &str = "ext:ark-bus";

/// Pipe endpoint name for the rebind dispatcher (T-6.4). Used by the
/// `reload_scene` keybind-diff path: the scene reloader sends a JSON
/// document of bindings to add and remove, and ark-bus invokes
/// [`zellij_tile::shim::rebind_keys`] to apply them at runtime
/// without restarting the zellij session.
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

/// Parsed shape of an `ark-rebind` payload (T-6.4).
///
/// The wire format is a JSON object with three top-level keys:
///
/// ```json
/// {
///   "unbind":   [ { "mode": "Normal", "key": "Alt p" }, … ],
///   "rebind":   [ { "mode": "Normal", "key": "Alt p", "actions": [ … ] }, … ],
///   "write_to_disk": false
/// }
/// ```
///
/// `unbind` and `rebind` are both optional; an empty array (or absence)
/// means "no changes in that direction". `write_to_disk` defaults to
/// `false` — the `reload_scene` driver should pass `true` only when the
/// user explicitly opts into mutating their on-disk zellij config.
///
/// Action shapes are kept opaque at this layer — see [`RebindActionSpec`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RebindRequest {
    /// Bindings to remove. Each entry pairs an `InputMode` name with a
    /// chord string parsable by `KeyWithModifier::from_str`.
    #[serde(default)]
    pub unbind: Vec<RebindKey>,
    /// Bindings to install. Each entry adds a chord+actions tuple to
    /// the named mode.
    #[serde(default)]
    pub rebind: Vec<RebindBinding>,
    /// Whether to persist the resulting key map to the user's on-disk
    /// zellij config. Defaults to `false` (in-memory only).
    #[serde(default)]
    pub write_to_disk: bool,
}

/// Key entry in the `unbind` half of [`RebindRequest`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RebindKey {
    /// `InputMode` name, e.g. `"Normal"`, `"Locked"`, `"Tab"`.
    pub mode: String,
    /// Chord string, e.g. `"Alt p"`, `"Ctrl Shift t"`. Parsed via
    /// `KeyWithModifier::from_str` on the wasm side.
    pub key: String,
}

/// Binding entry in the `rebind` half of [`RebindRequest`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RebindBinding {
    /// `InputMode` name.
    pub mode: String,
    /// Chord string.
    pub key: String,
    /// Ordered list of actions to fire when the chord is pressed.
    pub actions: Vec<RebindActionSpec>,
}

/// Action specification accepted by the rebind endpoint.
///
/// v1 supports a single shape: `MessagePlugin` (the zellij action that
/// becomes `KeybindPipe` internally) — this is what scene-compiled
/// keybinds emit to dispatch through ark-bus, so the rebind path can
/// install / replace them without needing a richer action grammar yet.
/// `kind` MUST be the literal string `"MessagePlugin"`.
///
/// Future tiers may grow other action kinds (`SwitchToMode`, `Quit`,
/// `Run`, …) as the scene grammar surfaces them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RebindActionSpec {
    /// Action discriminator. v1 accepts `"MessagePlugin"`.
    pub kind: String,
    /// Plugin url / name to message (e.g. `"ark-bus"`).
    pub plugin: String,
    /// Optional message name (e.g. `"ark-intent"`). Defaults to the
    /// plugin name when absent — matches zellij's own MessagePlugin
    /// fallback (see `zellij_utils::kdl::mod.rs::MessagePlugin`).
    #[serde(default)]
    pub name: Option<String>,
    /// Optional payload string. Verbatim — typically a JSON document
    /// the receiving plugin re-parses.
    #[serde(default)]
    pub payload: Option<String>,
}

/// Parse an `ark-rebind` payload, returning the structured request.
///
/// Mirrors [`validate_intent_payload`]'s philosophy — keep validation
/// loose at the plugin layer; the actual chord / action parsing
/// happens inside the wasm dispatcher where the zellij types are
/// available. Errors here cover only structural / shape problems.
pub fn validate_rebind_payload(payload: &str) -> Result<RebindRequest, RebindPayloadError> {
    let req: RebindRequest = serde_json::from_str(payload)
        .map_err(|e| RebindPayloadError::BadJson(e.to_string()))?;
    if req.unbind.is_empty() && req.rebind.is_empty() {
        return Err(RebindPayloadError::Empty);
    }
    for action_list in req.rebind.iter().flat_map(|b| &b.actions) {
        if action_list.kind != "MessagePlugin" {
            return Err(RebindPayloadError::UnsupportedActionKind(
                action_list.kind.clone(),
            ));
        }
    }
    Ok(req)
}

/// Errors returned by [`validate_rebind_payload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebindPayloadError {
    /// Top-level JSON failed to parse against [`RebindRequest`].
    BadJson(String),
    /// Both `unbind` and `rebind` arrays are empty / absent — nothing
    /// to do. We refuse rather than silently no-op so misconfigured
    /// callers see the issue.
    Empty,
    /// An action's `kind` is not in the v1 supported set
    /// (`MessagePlugin`).
    UnsupportedActionKind(String),
}

impl std::fmt::Display for RebindPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RebindPayloadError::BadJson(msg) => {
                write!(f, "ark-rebind payload is not valid JSON: {msg}")
            }
            RebindPayloadError::Empty => {
                write!(f, "ark-rebind payload has empty unbind and rebind arrays")
            }
            RebindPayloadError::UnsupportedActionKind(kind) => write!(
                f,
                "ark-rebind action kind `{kind}` is not supported in v1 (only `MessagePlugin`)"
            ),
        }
    }
}

/// Build the `--json` payload that `ark-hook emit` consumes when
/// forwarding a zellij-side event onto the ark event bus (T-6.3).
///
/// Schema: `{"event": "<name>", "payload": <map>, "source": "ext:ark-bus"}`.
/// `event` carries the canonical `ark.zellij.<kind>` name; `payload`
/// holds whatever event-specific fields the kind emits (terminal pane
/// id, exit code, file paths, …). The `source` is pinned to
/// [`FORWARDED_EVENT_SOURCE`] so reaction telemetry attributes the
/// broadcast to ark-bus, not the user scene.
///
/// Caller passes the bare `kind` string (e.g. `"command_pane_exited"`)
/// — this helper splices the prefix.
pub fn build_emit_payload(kind: &str, payload: serde_json::Value) -> String {
    let envelope = serde_json::json!({
        "event": format!("{FORWARDED_EVENT_PREFIX}{kind}"),
        "payload": payload,
        "source": FORWARDED_EVENT_SOURCE,
    });
    envelope.to_string()
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
    use super::{
        ArkBus, IntentPayloadError, PIPE_INTENT, PIPE_REBIND, PLUGIN_NAME,
        RebindPayloadError, build_emit_payload, validate_intent_payload, validate_rebind_payload,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::str::FromStr;
    use zellij_tile::prelude::*;
    // `Action` lives in `zellij_utils::input::actions` and is re-exported
    // (as the `actions` module) by zellij_tile's prelude. Pull the bare
    // type into scope so the `dispatch_rebind` translation reads
    // cleanly.
    use zellij_tile::prelude::actions::Action;

    impl ZellijPlugin for ArkBus {
        /// Lifecycle entry. ark-bus needs:
        ///   * `ReadCliPipes` — receive `MessagePlugin` payloads posted
        ///     by scene-compiled keybinds (T-6.2 / T-6.4).
        ///   * `RunCommands` — spawn the hidden `ark-hook` panes that
        ///     bridge into the supervisor control socket
        ///     (T-6.2 / T-6.3).
        ///   * `ChangeApplicationState` — required by zellij to receive
        ///     pane-lifecycle events (`CommandPaneOpened`,
        ///     `CommandPaneExited`, `PaneClosed`); see zellij-tile docs.
        ///
        /// And subscribes to the four canonical zellij-side events
        /// forwarded onto the ark event bus per T-6.3:
        ///   * `CommandPaneOpened`
        ///   * `CommandPaneExited`
        ///   * `PaneClosed`
        ///   * `FileSystemUpdate`
        fn load(&mut self, _configuration: BTreeMap<String, String>) {
            eprintln!("{PLUGIN_NAME}: load");
            request_permission(&[
                PermissionType::ReadCliPipes,
                PermissionType::RunCommands,
                PermissionType::ChangeApplicationState,
            ]);
            subscribe(&[
                EventType::CommandPaneOpened,
                EventType::CommandPaneExited,
                EventType::PaneClosed,
                EventType::FileSystemUpdate,
            ]);
        }

        /// Forward a subscribed zellij event onto the ark event bus by
        /// spawning a hidden `ark-hook emit --json '<json>'` command
        /// pane (T-6.3). Unmatched events are silently ignored — we
        /// only act on the four we explicitly subscribed to.
        fn update(&mut self, event: Event) -> bool {
            match event {
                Event::CommandPaneOpened(pane_id, _ctx) => {
                    let payload = serde_json::json!({
                        "terminal_pane_id": pane_id,
                    });
                    spawn_emit("command_pane_opened", payload);
                }
                Event::CommandPaneExited(pane_id, exit_code, _ctx) => {
                    let payload = serde_json::json!({
                        "terminal_pane_id": pane_id,
                        "exit_code": exit_code,
                    });
                    spawn_emit("command_pane_exited", payload);
                }
                Event::PaneClosed(pane_id) => {
                    // PaneId is an enum (`Terminal(u32) | Plugin(u32)`);
                    // serialise via Debug for now — the supervisor
                    // re-parses the payload through serde_json::Value
                    // and downstream consumers (scene reactions) match
                    // on the kind name, not the inner shape.
                    let payload = serde_json::json!({
                        "pane_id": format!("{pane_id:?}"),
                    });
                    spawn_emit("pane_closed", payload);
                }
                Event::FileSystemUpdate(updates) => {
                    let paths: Vec<String> = updates
                        .iter()
                        .map(|(p, _meta)| p.display().to_string())
                        .collect();
                    let payload = serde_json::json!({ "paths": paths });
                    spawn_emit("file_system_update", payload);
                }
                _ => {}
            }
            // No render needed — ark-bus is headless.
            false
        }

        /// Dispatch a pipe message. T-6.2 handles `ark-intent` by
        /// spawning a hidden command pane; T-6.4 handles `ark-rebind`
        /// by calling `zellij_tile::shim::rebind_keys` directly.
        fn pipe(&mut self, msg: PipeMessage) -> bool {
            match msg.name.as_str() {
                PIPE_INTENT => dispatch_intent(&msg),
                PIPE_REBIND => dispatch_rebind(&msg),
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

    /// Spawn `ark-hook emit --json '<envelope>'` in a hidden command
    /// pane to broadcast a forwarded zellij event onto the ark event
    /// bus (T-6.3). The `kind` is appended to
    /// `super::FORWARDED_EVENT_PREFIX` to form the canonical user-event
    /// name; `payload` is the kind-specific JSON object that scene
    /// reactions can read via the CEL `payload` binding.
    fn spawn_emit(kind: &str, payload: serde_json::Value) {
        let envelope = build_emit_payload(kind, payload);
        let cmd = CommandToRun {
            path: PathBuf::from("ark-hook"),
            args: vec!["emit".to_string(), "--json".to_string(), envelope],
            cwd: None,
        };
        let _ = open_command_pane_background(cmd, BTreeMap::new());
    }

    /// Apply a parsed `ark-rebind` payload via
    /// [`zellij_tile::shim::rebind_keys`]. Returns `false` (no plugin
    /// re-render needed). Errors are logged to stderr — there is no
    /// reply channel back to the supervisor for keybind diffs in v1.
    fn dispatch_rebind(msg: &PipeMessage) -> bool {
        let payload = match msg.payload.as_deref() {
            Some(p) => p,
            None => {
                eprintln!("{PLUGIN_NAME}: ark-rebind message has no payload; ignoring");
                return false;
            }
        };
        let req = match validate_rebind_payload(payload) {
            Ok(r) => r,
            Err(RebindPayloadError::BadJson(m)) => {
                eprintln!("{PLUGIN_NAME}: ark-rebind payload not valid JSON: {m}");
                return false;
            }
            Err(e) => {
                eprintln!("{PLUGIN_NAME}: ark-rebind payload rejected: {e}");
                return false;
            }
        };

        // Translate the wire shapes into zellij types. Failures here
        // are individual-binding errors — we log and skip the bad
        // binding rather than abort the whole batch.
        let mut to_unbind: Vec<(InputMode, KeyWithModifier)> = Vec::new();
        for u in &req.unbind {
            match (parse_input_mode(&u.mode), parse_chord(&u.key)) {
                (Some(mode), Some(key)) => to_unbind.push((mode, key)),
                _ => eprintln!(
                    "{PLUGIN_NAME}: ark-rebind unbind entry rejected (mode={:?}, key={:?})",
                    u.mode, u.key
                ),
            }
        }

        let mut to_rebind: Vec<(InputMode, KeyWithModifier, Vec<Action>)> = Vec::new();
        for b in &req.rebind {
            let mode = match parse_input_mode(&b.mode) {
                Some(m) => m,
                None => {
                    eprintln!(
                        "{PLUGIN_NAME}: ark-rebind unknown InputMode `{}`; skipping",
                        b.mode
                    );
                    continue;
                }
            };
            let key = match parse_chord(&b.key) {
                Some(k) => k,
                None => {
                    eprintln!(
                        "{PLUGIN_NAME}: ark-rebind chord `{}` rejected; skipping",
                        b.key
                    );
                    continue;
                }
            };
            let actions: Vec<Action> = b
                .actions
                .iter()
                .map(action_spec_to_action)
                .collect();
            to_rebind.push((mode, key, actions));
        }

        rebind_keys(to_unbind, to_rebind, req.write_to_disk);
        false
    }

    /// Parse an `InputMode` name. Wraps `InputMode::from_str`.
    fn parse_input_mode(s: &str) -> Option<InputMode> {
        InputMode::from_str(s).ok()
    }

    /// Parse a chord string via `KeyWithModifier::from_str`.
    fn parse_chord(s: &str) -> Option<KeyWithModifier> {
        KeyWithModifier::from_str(s).ok()
    }

    /// Translate an [`super::RebindActionSpec`] to a zellij `Action`.
    /// Today only `MessagePlugin` is supported (becomes
    /// `Action::KeybindPipe`); validation upstream guarantees `kind` is
    /// already `"MessagePlugin"`.
    fn action_spec_to_action(spec: &super::RebindActionSpec) -> Action {
        let plugin = spec.plugin.clone();
        let name = spec
            .name
            .clone()
            .unwrap_or_else(|| plugin.clone());
        Action::KeybindPipe {
            name: Some(name),
            payload: spec.payload.clone(),
            args: None,
            plugin: Some(plugin),
            configuration: None,
            launch_new: false,
            skip_cache: false,
            floating: Some(false),
            in_place: None,
            cwd: None,
            pane_title: None,
            plugin_id: None,
        }
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

    // ---- T-6.4: rebind payload validators ----

    #[test]
    fn validate_rebind_payload_accepts_unbind_only() {
        let req = validate_rebind_payload(
            r#"{"unbind":[{"mode":"Normal","key":"Alt p"}]}"#,
        )
        .expect("ok");
        assert_eq!(req.unbind.len(), 1);
        assert_eq!(req.rebind.len(), 0);
        assert!(!req.write_to_disk);
        assert_eq!(req.unbind[0].mode, "Normal");
        assert_eq!(req.unbind[0].key, "Alt p");
    }

    #[test]
    fn validate_rebind_payload_accepts_rebind_with_message_plugin() {
        let req = validate_rebind_payload(
            r#"{
                "rebind":[{
                    "mode":"Normal",
                    "key":"Alt s",
                    "actions":[{
                        "kind":"MessagePlugin",
                        "plugin":"ark-bus",
                        "name":"ark-intent",
                        "payload":"{\"name\":\"ark.core.open_tab\",\"args\":{}}"
                    }]
                }],
                "write_to_disk": true
            }"#,
        )
        .expect("ok");
        assert_eq!(req.rebind.len(), 1);
        assert!(req.write_to_disk);
        let b = &req.rebind[0];
        assert_eq!(b.mode, "Normal");
        assert_eq!(b.key, "Alt s");
        assert_eq!(b.actions.len(), 1);
        assert_eq!(b.actions[0].kind, "MessagePlugin");
        assert_eq!(b.actions[0].plugin, "ark-bus");
        assert_eq!(b.actions[0].name.as_deref(), Some("ark-intent"));
    }

    #[test]
    fn validate_rebind_payload_rejects_empty() {
        let err = validate_rebind_payload(r#"{}"#).expect_err("must error");
        assert_eq!(err, RebindPayloadError::Empty);
    }

    #[test]
    fn validate_rebind_payload_rejects_bad_json() {
        let err = validate_rebind_payload(r#"{not json"#).expect_err("must error");
        assert!(matches!(err, RebindPayloadError::BadJson(_)));
    }

    #[test]
    fn validate_rebind_payload_rejects_unsupported_action_kind() {
        let err = validate_rebind_payload(
            r#"{"rebind":[{"mode":"Normal","key":"Alt p","actions":[{"kind":"Quit","plugin":"x"}]}]}"#,
        )
        .expect_err("must error");
        match err {
            RebindPayloadError::UnsupportedActionKind(k) => assert_eq!(k, "Quit"),
            other => panic!("expected UnsupportedActionKind, got {other:?}"),
        }
    }

    #[test]
    fn rebind_payload_error_display_is_useful() {
        for e in [
            RebindPayloadError::BadJson("eof".to_string()),
            RebindPayloadError::Empty,
            RebindPayloadError::UnsupportedActionKind("Quit".to_string()),
        ] {
            let s = e.to_string();
            assert!(!s.is_empty());
            assert!(s.contains("ark-rebind"));
        }
    }

    // ---- T-6.3: emit payload builder ----

    #[test]
    fn build_emit_payload_envelopes_kind_and_payload() {
        let payload =
            serde_json::json!({ "terminal_pane_id": 7, "exit_code": 0 });
        let envelope = build_emit_payload("command_pane_exited", payload);
        let parsed: serde_json::Value =
            serde_json::from_str(&envelope).expect("ok");
        assert_eq!(
            parsed["event"],
            serde_json::Value::String("ark.zellij.command_pane_exited".into())
        );
        assert_eq!(
            parsed["source"],
            serde_json::Value::String(FORWARDED_EVENT_SOURCE.into())
        );
        assert_eq!(parsed["payload"]["terminal_pane_id"], serde_json::json!(7));
        assert_eq!(parsed["payload"]["exit_code"], serde_json::json!(0));
    }

    #[test]
    fn build_emit_payload_supports_arbitrary_payload_shapes() {
        let envelope = build_emit_payload(
            "file_system_update",
            serde_json::json!({ "paths": ["/a", "/b"] }),
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&envelope).expect("ok");
        assert_eq!(
            parsed["event"],
            serde_json::Value::String("ark.zellij.file_system_update".into())
        );
        assert_eq!(parsed["payload"]["paths"][0], serde_json::json!("/a"));
        assert_eq!(parsed["payload"]["paths"][1], serde_json::json!("/b"));
    }

    #[test]
    fn forwarded_event_constants_are_stable() {
        assert_eq!(FORWARDED_EVENT_PREFIX, "ark.zellij.");
        assert_eq!(FORWARDED_EVENT_SOURCE, "ext:ark-bus");
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
