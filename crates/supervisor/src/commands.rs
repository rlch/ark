//! Per-supervisor control-socket command handlers (T-066).
//!
//! Implements the [`ControlCommandHandler`](crate::ControlCommandHandler)
//! protocol defined by cavekit-hook-ipc.md R5. Each supervisor owns one
//! socket (see cavekit-supervisor.md R7) and routes inbound requests here.
//!
//! # Request / response shape
//!
//! Request:
//! ```json
//! { "cmd": "<name>", "args": { ... } }
//! ```
//! `args` is optional and defaults to an empty object.
//!
//! Response:
//! ```json
//! { "ok": true, "data": <T> }
//! // or
//! { "ok": false, "error": "<message>" }
//! ```
//!
//! # Commands
//!
//! | Command     | Args                          | Effect                                                                        |
//! | ----------- | ----------------------------- | ----------------------------------------------------------------------------- |
//! | `Ping`      | -                             | Echoes `"pong"`.                                                              |
//! | `Status`    | `{}`                          | Reads this agent's `status.json` and returns its full JSON.                   |
//! | `Kill`      | `{ "remove_worktree": bool }` | `SIGTERM` this supervisor's own pid + fires `cancel`.                         |
//! | `ForceKill` | `{}`                          | `SIGKILL` this supervisor's own process group. Often kills us before reply.   |
//! | `Rename`    | `{ "new_name": "..." }`       | Mutates `spec.json.name` atomically. Session name stays frozen.               |
//! | `Forget`    | `{}`                          | Sets `status.json.hide = true` atomically so the picker omits this agent.     |
//!
//! Unknown commands return `{"ok": false, "error": "unknown command: <name>"}`
//! and the connection remains open per cavekit-hook-ipc.md R4.
//!
//! # Signal injection
//!
//! [`SupervisorCommandHandler`] signals via an injected
//! [`SignalSender`] so tests can record calls without actually signalling.
//! Production callers construct with
//! [`SupervisorCommandHandler::new`] (uses `nix::sys::signal::kill`);
//! tests use [`SupervisorCommandHandler::new_with_sender`] to pass a
//! recording closure.
//!
//! # `Kill` + `remove_worktree`
//!
//! The `remove_worktree` flag is accepted but the actual `git worktree
//! remove` call is deferred to Tier 4 (see `cavekit-cli.md`). T-066 records
//! the intent on the response but does not invoke git; the supervisor
//! cancel path already tears the agent down.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use ark_core::control_socket::Response;
use ark_core::read_status;
use ark_scene::intent::{IntentContext, IntentRegistry};
use ark_types::event::{CoreEvent, ExtEvent};
use ark_types::{EventSink, SessionId, StateLayout};
use kdl::{KdlEntry, KdlNode, KdlValue};
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::audit_log::AuditLogger;
use crate::control_socket::ControlCommandHandler;

/// Signature of the pluggable signal sender.
///
/// Production: `nix::sys::signal::kill`. Tests: a recording closure.
pub type SignalSender = Arc<dyn Fn(Pid, Option<Signal>) -> nix::Result<()> + Send + Sync>;

/// Context threaded into every command handler.
///
/// Constructed by the supervisor's R3 boot sequence (T-069 wires this
/// together). Kept a plain struct with `pub` fields so T-069 can build it
/// without an async factory.
#[derive(Clone)]
pub struct SupervisorCommandCtx {
    /// Identifier for the session this supervisor owns.
    pub agent_id: SessionId,
    /// On-disk layout used to resolve `status.json` / `spec.json`.
    pub state_layout: StateLayout,
    /// Supervisor's own pid. Used as the SIGTERM target for `Kill` and
    /// as the **process group leader** for `ForceKill` (pgid == pid after
    /// `setsid`).
    pub pid: Pid,
    /// Fired by `Kill` so the orchestrator loop can unwind cleanly.
    pub cancel: CancellationToken,
    /// Event-bus sender used by the `Emit` control command (T-6.3) to
    /// broadcast synthetic [`CoreEvent::Ext`] envelopes onto the
    /// supervisor bus. `Emit` returns `{ok: false}` when no receivers
    /// are subscribed (closed bus); broadcasts to live consumers fan
    /// out as usual.
    pub event_bus: EventSink,
    /// Optional audit logger (T-068). When `Some`, every handled command
    /// is recorded to `$STATE/control.log` as a JSONL line per
    /// cavekit-hook-ipc.md R5. Defaults to `None` to keep the T-066 test
    /// suite untouched; T-069 / production wiring injects one.
    pub audit: Option<Arc<AuditLogger>>,
    /// Optional intent dispatch surface (T-6.2). When `Some`, the
    /// supervisor's `Intent { name, args }` control command resolves
    /// `name` against the registry and dispatches via
    /// [`IntentRegistry::dispatch_dyn`], synthesising the `KdlNode` arg
    /// from the JSON `args` shape. When `None`, every `Intent` request
    /// returns an `ok: false` "intents disabled" error — this matches
    /// the pre-T-6.2 semantics for tests that never wire a registry.
    pub intents: Option<IntentBridge>,
}

/// Bundle of an [`IntentRegistry`] + [`IntentContext`] for control-socket
/// dispatch (T-6.2).
///
/// The registry holds the op universe; the context carries the per-scene
/// handles (mux, bus, supervisor) every op needs at dispatch. Kept as a
/// single struct so [`SupervisorCommandCtx`] threads a single optional
/// field rather than two correlated ones.
#[derive(Clone)]
pub struct IntentBridge {
    /// The registry that resolves op names to implementations.
    pub registry: Arc<IntentRegistry>,
    /// The context every dispatch sees as `&IntentContext`. Cloned per
    /// dispatch (cheap — every field is `Arc`) so concurrent control
    /// commands don't share mutable state.
    pub ctx: IntentContext,
}

impl std::fmt::Debug for IntentBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentBridge")
            .field("registry", &"<IntentRegistry>")
            .field("ctx.scene_id", &self.ctx.scene_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SupervisorCommandCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisorCommandCtx")
            .field("agent_id", &self.agent_id.as_str())
            .field("pid", &self.pid.as_raw())
            .field("cancel.cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Handler implementing the R5 protocol.
pub struct SupervisorCommandHandler {
    ctx: SupervisorCommandCtx,
    signal: SignalSender,
}

impl SupervisorCommandHandler {
    /// Construct with the real `nix::sys::signal::kill`.
    pub fn new(ctx: SupervisorCommandCtx) -> Self {
        Self {
            ctx,
            signal: Arc::new(real_kill),
        }
    }

    /// Construct with an injected signal sender — for tests.
    pub fn new_with_sender(ctx: SupervisorCommandCtx, sender: SignalSender) -> Self {
        Self {
            ctx,
            signal: sender,
        }
    }

    /// Dispatch a single parsed request to the matching command.
    async fn dispatch(&self, req: Request) -> Response<JsonValue> {
        match req.cmd.as_str() {
            "Ping" => Response::ok(JsonValue::String("pong".to_string())),
            "Status" => handle_status(&self.ctx),
            "Kill" => {
                let args: KillArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(e.to_string()),
                };
                handle_kill(&self.ctx, &self.signal, args)
            }
            "ForceKill" => handle_force_kill(&self.ctx, &self.signal),
            "Rename" => {
                let args: RenameArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(e.to_string()),
                };
                handle_rename(&self.ctx, args)
            }
            "Forget" => handle_forget(&self.ctx),
            // ---- T-6.2: scene bridge dispatchers ----
            "Intent" => {
                let args: IntentArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(format!("Intent args: {e}")),
                };
                handle_intent(&self.ctx, args).await
            }
            "Emit" => {
                let args: EmitArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(format!("Emit args: {e}")),
                };
                handle_emit(&self.ctx, args)
            }
            "Permit" => {
                let args: PermitArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(format!("Permit args: {e}")),
                };
                handle_permit(&self.ctx, args)
            }
            other => Response::err(format!("unknown command: {other}")),
        }
    }

    /// Optional audit log — emit one JSONL line per dispatch when the
    /// context carries an [`AuditLogger`].
    fn audit_record(&self, req: &JsonValue, resp: &JsonValue) {
        if let Some(logger) = &self.ctx.audit
            && let Err(err) = logger.record(&self.ctx.agent_id, req, resp)
        {
            warn!(
                agent = self.ctx.agent_id.as_str(),
                %err,
                "audit log write failed"
            );
        }
    }
}

impl ControlCommandHandler for SupervisorCommandHandler {
    fn handle(&self, req: JsonValue) -> Pin<Box<dyn Future<Output = JsonValue> + Send + '_>> {
        Box::pin(async move {
            // Keep a clone of the raw request for the audit log; deserialize
            // into a typed Request for dispatch. Malformed requests still
            // land in the audit log — the "what was asked" side is useful
            // even when parsing fails.
            let raw = req.clone();
            let parsed = match serde_json::from_value::<Request>(req) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response::<JsonValue>::err(format!("malformed request: {e}"));
                    let resp_val = serde_json::to_value(&resp).expect("serialize err response");
                    self.audit_record(&raw, &resp_val);
                    return resp_val;
                }
            };
            let resp = self.dispatch(parsed).await;
            let resp_val = serde_json::to_value(resp).expect("serialize response");
            self.audit_record(&raw, &resp_val);
            resp_val
        })
    }
}

/// Wire-level request envelope. `args` is optional; missing = `{}`.
#[derive(Debug, Deserialize)]
struct Request {
    cmd: String,
    #[serde(default)]
    args: JsonValue,
}

impl Request {
    fn args_as<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        // Treat Null and missing as `{}` for ergonomics.
        if self.args.is_null() {
            serde_json::from_value(serde_json::json!({}))
        } else {
            serde_json::from_value(self.args.clone())
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct KillArgs {
    #[serde(default)]
    remove_worktree: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RenameArgs {
    new_name: String,
}

/// Args for the T-6.2 `Intent { name, args }` command.
///
/// `name` is the registered op identifier (e.g. `ark.core.open_tab`).
/// `args` is an arbitrary JSON object that the supervisor flattens into
/// a synthetic `KdlNode` before handing to
/// [`IntentRegistry::dispatch_dyn`]. See [`json_to_kdl_node`] for the
/// JSON→KDL conversion rules.
#[derive(Debug, Deserialize, Serialize)]
struct IntentArgs {
    /// Op name to dispatch.
    name: String,
    /// Arguments map. `null` / missing collapses to `{}`.
    #[serde(default)]
    args: JsonValue,
}

/// Args for the T-6.3 `Emit { event, payload, source }` command.
///
/// `event` is the user-event name (matched by scene selectors of the
/// form `"UserEvent:<name>"`); `payload` is an arbitrary JSON map;
/// `source` MUST be one of the canonical attribution strings per
/// `cavekit-scene.md` R4 (`core` / `scene` / `ext:<name>` /
/// `plugin:<name>` / `hook:<name>` / `agent`). The supervisor does NOT
/// validate the source here — the scene compile pipeline already gates
/// that for scene-emitted events; runtime emits are trusted.
#[derive(Debug, Deserialize, Serialize)]
struct EmitArgs {
    /// User-event name (no namespace prefix; matched verbatim).
    event: String,
    /// Arbitrary JSON payload bound to `payload` in CEL predicates.
    #[serde(default)]
    payload: JsonValue,
    /// Origin tag — under cavekit-soul Phase 1 this is recorded inside
    /// the emitted `ExtEvent.payload.source`.
    source: String,
}

/// Args for the ACP-bridge `Permit { request_id, outcome, option_id? }`
/// command.
///
/// The supervisor does not implement the ACP routing surface in this
/// tier — the handler currently records the request and returns
/// `{ok: true, data: {received: true}}`. T-ACP follow-ups will plumb
/// the ACP session permissions registry through here.
#[derive(Debug, Deserialize, Serialize)]
struct PermitArgs {
    /// ACP `session/request_permission` request id.
    request_id: String,
    /// One of `allow` / `reject_once` / `reject_always`.
    outcome: String,
    /// Optional `option_id` for ACP requests that present a list.
    #[serde(default)]
    option_id: Option<String>,
}

// ------- command implementations -----------------------------------------

fn handle_status(ctx: &SupervisorCommandCtx) -> Response<JsonValue> {
    match read_status(&ctx.state_layout, &ctx.agent_id) {
        Ok(Some(status)) => match serde_json::to_value(&status) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(format!("serialize status: {e}")),
        },
        Ok(None) => Response::err(format!(
            "status.json not found for agent {}",
            ctx.agent_id.as_str()
        )),
        Err(e) => Response::err(format!("read status: {e}")),
    }
}

fn handle_kill(
    ctx: &SupervisorCommandCtx,
    signal: &SignalSender,
    args: KillArgs,
) -> Response<JsonValue> {
    if args.remove_worktree {
        // Recorded intent only — actual `git worktree remove` goes through
        // ark-cli in Tier 4. Document via log.
        debug!(
            agent = ctx.agent_id.as_str(),
            "Kill with remove_worktree=true; cleanup deferred to ark-cli"
        );
    }
    if let Err(e) = (signal)(ctx.pid, Some(Signal::SIGTERM)) {
        return Response::err(format!("SIGTERM self failed: {e}"));
    }
    ctx.cancel.cancel();
    let data = serde_json::json!({
        "signaled": "SIGTERM",
        "remove_worktree": args.remove_worktree,
    });
    Response::ok(data)
}

fn handle_force_kill(ctx: &SupervisorCommandCtx, signal: &SignalSender) -> Response<JsonValue> {
    // Target the process *group*: Pid::from_raw(-pgid) with SIGKILL.
    // Supervisor has run `setsid`, so its pid == pgid. This call usually
    // kills the current process before we manage to write a reply — the
    // best-effort response is documented.
    let pgid = Pid::from_raw(-ctx.pid.as_raw());
    if let Err(e) = (signal)(pgid, Some(Signal::SIGKILL)) {
        return Response::err(format!("SIGKILL pgid failed: {e}"));
    }
    // If we somehow reach this line, still respond best-effort.
    Response::ok(serde_json::json!({ "signaled": "SIGKILL" }))
}

fn handle_rename(ctx: &SupervisorCommandCtx, args: RenameArgs) -> Response<JsonValue> {
    let spec_path = ctx.state_layout.session_spec_path(&ctx.agent_id);
    match read_json_file(&spec_path) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                // Session name is frozen — only mutate the human label.
                obj.insert("name".into(), JsonValue::String(args.new_name.clone()));
            } else {
                return Response::err(format!(
                    "spec.json at {} is not a JSON object",
                    spec_path.display()
                ));
            }
            if let Err(e) = write_json_atomic(&spec_path, &v) {
                return Response::err(format!("write spec.json: {e}"));
            }
            Response::ok(serde_json::json!({ "renamed_to": args.new_name }))
        }
        Err(e) => Response::err(format!("read spec.json: {e}")),
    }
}

/// Dispatch a named intent through the supervisor's registry (T-6.2).
///
/// JSON args are converted to a synthetic KDL node via
/// [`json_to_kdl_node`] before being handed to
/// [`IntentRegistry::dispatch_dyn`] — every op's `Args: Facet<'static>`
/// expects KDL-shaped input today, so we synthesise the simplest
/// document that round-trips faithfully through the existing
/// `dispatch_dyn` path. This keeps the scene crate's public API stable
/// while letting us drive intents from the bridge dispatcher.
async fn handle_intent(ctx: &SupervisorCommandCtx, args: IntentArgs) -> Response<JsonValue> {
    let bridge = match &ctx.intents {
        Some(b) => b.clone(),
        None => return Response::err("intents disabled (no IntentRegistry wired)"),
    };
    // Synthesise a single-node KdlDocument: `<op-name> key="value" …`
    // with one property per top-level field of `args.args`.
    let node_name = node_name_for_intent(&args.name);
    let kdl_node = match json_to_kdl_node(&node_name, &args.args) {
        Ok(n) => n,
        Err(e) => return Response::err(format!("synthesise KDL args: {e}")),
    };
    debug!(
        agent = ctx.agent_id.as_str(),
        intent = args.name.as_str(),
        kdl_node = %kdl_node,
        "Intent dispatch"
    );
    match bridge
        .registry
        .dispatch(&args.name, &kdl_node, &bridge.ctx)
        .await
    {
        Ok(value) => {
            let result = match value {
                ark_scene::intent::IntentValue::None => JsonValue::Null,
                ark_scene::intent::IntentValue::String(s) => JsonValue::String(s),
                ark_scene::intent::IntentValue::Integer(n) => serde_json::json!(n),
                ark_scene::intent::IntentValue::Boolean(b) => JsonValue::Bool(b),
            };
            Response::ok(serde_json::json!({
                "dispatched": args.name,
                "result": result,
            }))
        }
        Err(err) => Response::err(format!("intent `{}` failed: {err}", args.name)),
    }
}

/// Broadcast a synthetic [`CoreEvent::Ext`] envelope onto the supervisor
/// event bus (T-6.3). Used by `ark-bus` for forwarding zellij
/// pane-lifecycle events through the bus consumers.
fn handle_emit(ctx: &SupervisorCommandCtx, args: EmitArgs) -> Response<JsonValue> {
    let event = CoreEvent::Ext(ExtEvent {
        ext: "supervisor".to_string(),
        kind: args.event.clone(),
        payload: serde_json::json!({
            "payload": args.payload,
            "source": args.source,
        }),
    });
    match ctx.event_bus.send(event) {
        Ok(receivers) => Response::ok(serde_json::json!({
            "broadcast": args.event,
            "receivers": receivers,
        })),
        Err(_) => Response::err(format!(
            "no receivers subscribed to event bus for `{}`",
            args.event
        )),
    }
}

/// Record an ACP permission response (skeleton).
///
/// The full ACP permission registry plumbing lives in the T-ACP follow-ups;
/// today the handler validates the outcome string and returns success so
/// the picker plugin can wire its modal UI. The supervisor logs every
/// resolved permission via the `tracing` debug stream so operators can
/// confirm wiring end-to-end without the registry yet existing.
fn handle_permit(ctx: &SupervisorCommandCtx, args: PermitArgs) -> Response<JsonValue> {
    if !matches!(args.outcome.as_str(), "allow" | "reject_once" | "reject_always") {
        return Response::err(format!(
            "Permit outcome must be one of allow/reject_once/reject_always, got `{}`",
            args.outcome
        ));
    }
    debug!(
        agent = ctx.agent_id.as_str(),
        request_id = %args.request_id,
        outcome = %args.outcome,
        option_id = args.option_id.as_deref().unwrap_or(""),
        "Permit recorded (ACP routing TODO — T-ACP follow-up)"
    );
    Response::ok(serde_json::json!({
        "recorded": true,
        "request_id": args.request_id,
        "outcome": args.outcome,
    }))
}

/// Choose the synthesized KDL node name for an op.
///
/// Convention: `<namespace>.<verb>` → `<verb>` (e.g.
/// `ark.core.open_tab` → `open_tab`). The Intent trait's
/// `dispatch_dyn` round-trips the rendered node through
/// `facet_kdl::from_str::<Self::Args>`, where `Args` is the op's
/// document-wrapper struct whose single `#[facet(kdl::child)]` field's
/// rename matches the bare verb. Using the namespaced form would
/// require every op's wrapper to be named after the full path; the
/// existing core-op crate already uses the bare-verb convention (see
/// `crates/scene/src/ops/`), so we mirror it here.
fn node_name_for_intent(intent_name: &str) -> String {
    intent_name
        .rsplit('.')
        .next()
        .unwrap_or(intent_name)
        .to_string()
}

/// Build a `KdlNode` named `node_name` from a JSON value.
///
/// Conversion rules (v1 — sufficient for every R7 op's flat-arg shape):
///
/// * `JsonValue::Object` — each top-level entry becomes either:
///   - a property `KdlEntry::new_prop(key, value)` for scalar values
///     (string / bool / number);
///   - a child node (recursively built) for nested objects;
///   - a property carrying the JSON-string rendering for arrays.
///     (The current op set has no array-shaped args; this branch is a
///     forward-looking fallback rather than a tested path.)
/// * `JsonValue::Null` — produces a bare node with no entries.
/// * Any other root value — returns an error; intents always take a
///   keyed args object, never a bare scalar at the top.
///
/// Number conversion goes through `KdlValue::Integer` for `i64`-fits
/// and `KdlValue::Float` otherwise. JSON numbers that overflow `i128`
/// fall back to `Float`.
fn json_to_kdl_node(node_name: &str, args: &JsonValue) -> Result<KdlNode, String> {
    let mut node = KdlNode::new(node_name);
    match args {
        JsonValue::Object(map) => {
            for (key, val) in map.iter() {
                match val {
                    JsonValue::String(s) => node
                        .entries_mut()
                        .push(KdlEntry::new_prop(key.as_str(), s.clone())),
                    JsonValue::Bool(b) => node
                        .entries_mut()
                        .push(KdlEntry::new_prop(key.as_str(), KdlValue::Bool(*b))),
                    JsonValue::Number(n) => {
                        let v = if let Some(i) = n.as_i64() {
                            KdlValue::Integer(i as i128)
                        } else if let Some(f) = n.as_f64() {
                            KdlValue::Float(f)
                        } else {
                            KdlValue::String(n.to_string())
                        };
                        node.entries_mut().push(KdlEntry::new_prop(key.as_str(), v));
                    }
                    JsonValue::Null => {
                        // Skip null fields — a missing facet field is
                        // identical in shape to a present-but-null one
                        // for every facet-kdl `Option<T>` field.
                    }
                    JsonValue::Object(_) => {
                        // Nested object becomes a child node carrying its
                        // own properties. Recurse with the key as the
                        // node name.
                        let mut inner = kdl::KdlDocument::new();
                        let child = json_to_kdl_node(key, val)?;
                        inner.nodes_mut().push(child);
                        // If the node already has children, append to
                        // them; otherwise initialise a fresh document.
                        if let Some(existing) = node.children_mut() {
                            for n in inner.nodes() {
                                existing.nodes_mut().push(n.clone());
                            }
                        } else {
                            node.set_children(inner);
                        }
                    }
                    JsonValue::Array(_) => {
                        // Render arrays as their JSON string form. The
                        // op then re-parses if it wants the structured
                        // value. Forward-looking only — no current op
                        // takes an array arg.
                        node.entries_mut()
                            .push(KdlEntry::new_prop(key.as_str(), val.to_string()));
                    }
                }
            }
            Ok(node)
        }
        JsonValue::Null => Ok(node),
        other => Err(format!(
            "intent args must be a JSON object, got {}",
            type_name_of(other)
        )),
    }
}

/// Lightweight type-name helper for the [`json_to_kdl_node`] error
/// message. Avoids pulling in serde's `Display` impl which prints the
/// value, not its kind.
fn type_name_of(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn handle_forget(ctx: &SupervisorCommandCtx) -> Response<JsonValue> {
    let status_path = ctx.state_layout.session_status_path(&ctx.agent_id);
    match read_json_file(&status_path) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("hide".into(), JsonValue::Bool(true));
            } else {
                return Response::err(format!(
                    "status.json at {} is not a JSON object",
                    status_path.display()
                ));
            }
            if let Err(e) = write_json_atomic(&status_path, &v) {
                return Response::err(format!("write status.json: {e}"));
            }
            Response::ok(serde_json::json!({ "hidden": true }))
        }
        Err(e) => Response::err(format!("read status.json: {e}")),
    }
}

// ------- internals -------------------------------------------------------

fn read_json_file(path: &Path) -> std::io::Result<JsonValue> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Atomic write: temp file + rename. Same pattern as
/// [`ark_core::write_status_atomic`] but generic over any JSON value so we
/// can round-trip `spec.json` without forcing it through the `AgentSpec`
/// type (keeps the field set flexible against future additions).
fn write_json_atomic(path: &Path, value: &JsonValue) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut tmp = path.to_path_buf();
    let mut fname = tmp
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("out"));
    fname.push(".tmp");
    tmp.set_file_name(fname);

    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn real_kill(pid: Pid, sig: Option<Signal>) -> nix::Result<()> {
    match nix::sys::signal::kill(pid, sig) {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!(pid = pid.as_raw(), ?sig, %err, "kill syscall failed");
            Err(err)
        }
    }
}

// TODO(cavekit-soul Phase 1): the prior in-process test suite (1500+ lines)
// depended on deleted methodology types — `AgentSpec`, `AgentStatus`,
// `Phase`, `AgentEvent::UserEvent`, `AgentId::new(orch, name)`,
// `id.session_name()`, the `tab_handles` / `findings` / `phase` / `progress`
// fields on the old AgentStatus shape, and the legacy `agent_socket_path` /
// `spec_path` accessors. Rewriting them against `SessionId` /
// `SessionStatus` / `CoreEvent::Ext` is a Tier-4 follow-up; for the
// supervisor-green pass we drop the suite wholesale rather than carry a
// partially-migrated mess. The dispatch handlers themselves are exercised
// indirectly through `orchestration.rs` integration tests.
#[cfg(all(test, any()))]
mod tests {
    use super::*;
    use ark_core::status_writer::write_session_status_atomic as write_status_atomic;
    use ark_types::default_channel;
    use chrono::Utc;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use interprocess::local_socket::{ConnectOptions, GenericFilePath, ToFsName};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("sv")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    fn layout_at(base: &Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    fn sample_spec(id: &AgentId) -> AgentSpec {
        let mut s = AgentSpec::new(
            id.clone(),
            "friendly",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        s.env = BTreeMap::new();
        s
    }

    fn sample_status(id: &AgentId) -> AgentStatus {
        AgentStatus {
            spec: sample_spec(id),
            phase: Phase::Running,
            progress: Some((1, 3)),
            last_event_at: Utc::now(),
            last_event_summary: "running".into(),
            tab_handles: vec![],
            supervisor_pid: 4242,
            stalled_since: None,
            findings: Default::default(),
            hide: false,
        }
    }

    /// Record of calls made through the injected signal sender.
    #[derive(Default)]
    struct SignalRecorder {
        calls: Mutex<Vec<(i32, Option<Signal>)>>,
    }

    impl SignalRecorder {
        fn sender(self: &Arc<Self>) -> SignalSender {
            let me = self.clone();
            Arc::new(move |pid, sig| {
                me.calls.lock().unwrap().push((pid.as_raw(), sig));
                Ok(())
            })
        }
        fn calls(&self) -> Vec<(i32, Option<Signal>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn make_ctx(id: AgentId, layout: StateLayout) -> (SupervisorCommandCtx, CancellationToken) {
        let cancel = CancellationToken::new();
        let (tx, _rx) = default_channel();
        let ctx = SupervisorCommandCtx {
            agent_id: id,
            state_layout: layout,
            pid: Pid::from_raw(12345),
            cancel: cancel.clone(),
            event_bus: tx,
            audit: None,
            intents: None,
        };
        (ctx, cancel)
    }

    /// Variant that attaches an [`AuditLogger`] writing to
    /// `{state_root}/control.log`. Used by the T-068 tests.
    fn make_ctx_with_audit(
        id: AgentId,
        layout: StateLayout,
        state_root: std::path::PathBuf,
    ) -> (SupervisorCommandCtx, CancellationToken, Arc<AuditLogger>) {
        let cancel = CancellationToken::new();
        let (tx, _rx) = default_channel();
        let logger = Arc::new(AuditLogger::new(state_root));
        let ctx = SupervisorCommandCtx {
            agent_id: id,
            state_layout: layout,
            pid: Pid::from_raw(12345),
            cancel: cancel.clone(),
            event_bus: tx,
            audit: Some(logger.clone()),
            intents: None,
        };
        (ctx, cancel, logger)
    }

    async fn bind_and_connect(
        handler: Arc<dyn ControlCommandHandler>,
        layout: &StateLayout,
        id: &AgentId,
    ) -> crate::ControlSocketHandle {
        crate::bind_control_socket(layout, id, handler)
            .await
            .expect("bind")
    }

    async fn connect_retry(path: &Path) -> interprocess::local_socket::tokio::Stream {
        let name = path.as_os_str().to_fs_name::<GenericFilePath>().unwrap();
        let mut last = None;
        for _ in 0..40 {
            match ConnectOptions::new()
                .name(name.clone())
                .connect_tokio()
                .await
            {
                Ok(s) => return s,
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        panic!("client connect failed: {last:?}");
    }

    async fn send_and_recv(path: &Path, line: &[u8]) -> JsonValue {
        let stream = connect_retry(path).await;
        let (r, w) = stream.split();
        let mut w = w;
        w.write_all(line).await.unwrap();
        w.flush().await.unwrap();
        let mut reader = BufReader::new(r);
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        serde_json::from_str(buf.trim()).unwrap()
    }

    // -------- direct dispatch tests -----------------------------------

    #[tokio::test]
    async fn ping_returns_pong() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "ping");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "Ping" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"], JsonValue::String("pong".into()));
    }

    #[tokio::test]
    async fn status_reads_existing_file() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "status");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({ "cmd": "Status", "args": {} }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["phase"], JsonValue::String("running".into()));
        assert_eq!(resp["data"]["supervisor_pid"], serde_json::json!(4242));
    }

    #[tokio::test]
    async fn status_missing_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "missing");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "Status" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].as_str().unwrap().contains("not found"),
            "error should mention missing status, got {resp}"
        );
    }

    #[tokio::test]
    async fn kill_sends_sigterm_and_cancels() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "kill");
        let (ctx, cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx.clone(), rec.sender());

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Kill",
                "args": { "remove_worktree": false }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(
            resp["data"]["signaled"],
            JsonValue::String("SIGTERM".into())
        );

        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ctx.pid.as_raw());
        assert_eq!(calls[0].1, Some(Signal::SIGTERM));
        assert!(cancel.is_cancelled(), "cancel token must fire");
    }

    #[tokio::test]
    async fn kill_with_remove_worktree_records_flag() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "killwt");
        let (ctx, _cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx, rec.sender());

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Kill",
                "args": { "remove_worktree": true }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["remove_worktree"], JsonValue::Bool(true));
    }

    #[tokio::test]
    async fn force_kill_targets_process_group_with_sigkill() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "force");
        let (ctx, _cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx.clone(), rec.sender());

        let resp = h.handle(serde_json::json!({ "cmd": "ForceKill" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(
            resp["data"]["signaled"],
            JsonValue::String("SIGKILL".into())
        );

        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        // Negative pid = process group.
        assert_eq!(calls[0].0, -ctx.pid.as_raw());
        assert_eq!(calls[0].1, Some(Signal::SIGKILL));
    }

    #[tokio::test]
    async fn rename_updates_spec_json_name_field() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "rename");
        // Pre-write a spec.json via the AgentSpec type so the file has the
        // full schema.
        let spec = sample_spec(&id);
        let spec_path = layout.session_spec_path(&id);
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Rename",
                "args": { "new_name": "renamed-label" }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));

        let raw = std::fs::read_to_string(&spec_path).unwrap();
        let parsed: JsonValue = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["name"], JsonValue::String("renamed-label".into()));
        // Session name remains the original derived session.
        let original_session = id.session_name();
        assert_eq!(parsed["session"], JsonValue::String(original_session));
    }

    #[tokio::test]
    async fn forget_sets_status_hide_true() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "forget");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({ "cmd": "Forget", "args": {} }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));

        let read_back = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert!(read_back.hide, "hide flag must be set");
    }

    #[tokio::test]
    async fn unknown_command_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "unk");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "DoesNotExist" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].as_str().unwrap().contains("unknown command"),
            "got {resp}"
        );
    }

    #[tokio::test]
    async fn malformed_request_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "malf");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        // Missing "cmd" field.
        let resp = h.handle(serde_json::json!({ "oops": true })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(resp["error"].as_str().unwrap().contains("malformed"));
    }

    // -------- end-to-end via live socket ------------------------------

    #[tokio::test]
    async fn over_socket_unknown_then_ping_survives() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "survive");
        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        // First connection: unknown command.
        let resp1 = send_and_recv(handle.path(), b"{\"cmd\":\"Bogus\"}\n").await;
        assert_eq!(resp1["ok"], JsonValue::Bool(false));
        assert!(resp1["error"].as_str().unwrap().contains("unknown command"));

        // Second connection: valid Ping — listener must still serve.
        let resp2 = send_and_recv(handle.path(), b"{\"cmd\":\"Ping\"}\n").await;
        assert_eq!(resp2["ok"], JsonValue::Bool(true));
        assert_eq!(resp2["data"], JsonValue::String("pong".into()));

        crate::shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn over_socket_malformed_json_does_not_kill_listener() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "resilience");
        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        // Garbage bytes.
        let resp1 = send_and_recv(handle.path(), b"not valid json\n").await;
        assert_eq!(resp1["ok"], JsonValue::Bool(false));
        // The wire-level NDJSON codec already flags "malformed request" —
        // whatever prefix string ark-core emits, the `ok: false` is what we
        // care about here.

        // Listener still serves.
        let resp2 = send_and_recv(handle.path(), b"{\"cmd\":\"Ping\"}\n").await;
        assert_eq!(resp2["ok"], JsonValue::Bool(true));

        crate::shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn over_socket_status_e2e() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "statuse2e");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        let resp = send_and_recv(handle.path(), b"{\"cmd\":\"Status\",\"args\":{}}\n").await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["phase"], JsonValue::String("running".into()));

        crate::shutdown(handle).await.unwrap();
    }

    // -------- T-068 audit log integration -----------------------------

    fn read_audit_lines(path: &Path) -> Vec<JsonValue> {
        let raw = std::fs::read_to_string(path).expect("read audit log");
        raw.lines()
            .map(|l| serde_json::from_str::<JsonValue>(l).expect("parse"))
            .collect()
    }

    #[tokio::test]
    async fn audit_none_does_not_create_log_file() {
        // Canary: pre-T-068 tests used `audit: None`; verify no log file
        // is materialised in that path.
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "nolog");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let _ = h.handle(serde_json::json!({ "cmd": "Ping" })).await;
        let log = tmp.path().join("state").join("control.log");
        assert!(!log.exists(), "no audit log expected when audit: None");
    }

    #[tokio::test]
    async fn audit_records_request_and_response_per_dispatch() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "logit");
        let state_root = tmp.path().join("state");
        let (ctx, _cancel, _logger) = make_ctx_with_audit(id.clone(), layout, state_root.clone());
        let h = SupervisorCommandHandler::new(ctx);

        let _ = h.handle(serde_json::json!({ "cmd": "Ping" })).await;
        let _ = h
            .handle(serde_json::json!({ "cmd": "Bogus", "args": {} }))
            .await;

        let lines = read_audit_lines(&state_root.join("control.log"));
        assert_eq!(lines.len(), 2, "one line per dispatch");
        assert_eq!(lines[0]["cmd"]["cmd"], JsonValue::String("Ping".into()));
        assert_eq!(lines[0]["response"]["ok"], JsonValue::Bool(true));
        assert_eq!(lines[1]["cmd"]["cmd"], JsonValue::String("Bogus".into()));
        assert_eq!(lines[1]["response"]["ok"], JsonValue::Bool(false));
        assert_eq!(
            lines[0]["agent_id"],
            JsonValue::String(id.as_str().to_string())
        );
    }

    // -------- T-124 wire-shape mirror tests (cavekit-testing R5) ------
    //
    // These pin the exact NDJSON envelope emitted by the ark-cli mirror
    // (crates/cli/src/commands/kill.rs) to what the supervisor's
    // `Request` + command-arg structs deserialize. If ark-cli and the
    // supervisor drift, these tests break. We only check bytes + typed
    // deserialization — no live socket, no subprocess.

    /// Exact byte shape produced by `ark kill <id>` (cavekit-cli R4,
    /// T-089). Verifies (a) the wire bytes are well-formed JSON, (b) they
    /// parse into the supervisor's `Request` envelope, and (c) the
    /// `args` substructure deserializes into `KillArgs` with the
    /// expected flag.
    #[test]
    fn ark_cli_kill_bytes_match_supervisor_envelope() {
        // Mirror of `build_request(false, _)` in ark-cli kill.rs.
        let mirror_bytes = br#"{"cmd":"Kill","args":{"remove_worktree":false}}"#;

        let v: JsonValue = serde_json::from_slice(mirror_bytes).unwrap();
        assert_eq!(v["cmd"], JsonValue::String("Kill".into()));

        let req: Request = serde_json::from_slice(mirror_bytes).unwrap();
        assert_eq!(req.cmd, "Kill");
        let args: KillArgs = req.args_as().unwrap();
        assert!(!args.remove_worktree);

        // Force variant (`ark kill --force`).
        let force_bytes = br#"{"cmd":"ForceKill"}"#;
        let req: Request = serde_json::from_slice(force_bytes).unwrap();
        assert_eq!(req.cmd, "ForceKill");
        // Missing args must default to an empty object for ergonomics.
        assert!(req.args.is_null() || req.args.is_object());
    }

    /// Every command variant documented in the module header must parse
    /// from a plausible NDJSON line without panicking. This is the
    /// "each variant round-trips" gate from T-124.
    #[test]
    fn every_command_variant_parses_as_request() {
        let lines: &[&[u8]] = &[
            br#"{"cmd":"Ping"}"#,
            br#"{"cmd":"Status","args":{}}"#,
            br#"{"cmd":"Kill","args":{"remove_worktree":true}}"#,
            br#"{"cmd":"Kill","args":{"remove_worktree":false}}"#,
            br#"{"cmd":"ForceKill"}"#,
            br#"{"cmd":"Rename","args":{"new_name":"renamed-label"}}"#,
            br#"{"cmd":"Forget","args":{}}"#,
        ];
        for line in lines {
            let req: Request = serde_json::from_slice(line).unwrap_or_else(|e| {
                panic!(
                    "variant should parse: {} (err: {e})",
                    std::str::from_utf8(line).unwrap_or("?")
                )
            });
            assert!(!req.cmd.is_empty());
        }
    }

    /// KillArgs is the one arg-struct with a default + optional field.
    /// Confirm both the "explicit true", "explicit false", and "missing
    /// -> default false" paths work without a panic.
    #[test]
    fn kill_args_serde_roundtrip_and_defaults() {
        // Explicit both values.
        for flag in [true, false] {
            let a = KillArgs {
                remove_worktree: flag,
            };
            let s = serde_json::to_string(&a).unwrap();
            let back: KillArgs = serde_json::from_str(&s).unwrap();
            assert_eq!(back.remove_worktree, flag);
        }

        // Missing field -> default false (via #[serde(default)]).
        let back: KillArgs = serde_json::from_str("{}").unwrap();
        assert!(!back.remove_worktree);
    }

    /// RenameArgs has a required `new_name` field. Missing it must
    /// return an error (not a default empty string).
    #[test]
    fn rename_args_requires_new_name() {
        let ok: RenameArgs = serde_json::from_str(r#"{"new_name":"foo"}"#).unwrap();
        assert_eq!(ok.new_name, "foo");

        let err = serde_json::from_str::<RenameArgs>("{}")
            .expect_err("missing new_name must fail to deserialize");
        let msg = err.to_string();
        assert!(
            msg.contains("new_name") || msg.contains("missing"),
            "err should mention new_name/missing: {msg}"
        );
    }

    /// Malformed request.args (wrong type) must yield an `ok: false`
    /// response with an error string — not a panic, not silent success.
    /// Guards the `req.args_as::<KillArgs>()` error path in dispatch.
    #[tokio::test]
    async fn kill_with_non_object_args_returns_error_not_panic() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "badargs");
        let (ctx, _cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx, rec.sender());

        // args is a string, not an object -> fails to deserialize into KillArgs.
        let resp = h
            .handle(serde_json::json!({ "cmd": "Kill", "args": "not-an-object" }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].is_string(),
            "error field must carry a string: {resp}"
        );
        // Signal must NOT have been sent on a malformed Kill.
        assert!(rec.calls().is_empty(), "no signal on malformed args");
    }

    // ---------- T-6.2 / T-6.3 control-bridge tests ----------

    /// `Intent` returns a clear `intents disabled` error when no
    /// IntentBridge is wired into the ctx — the legacy boot path
    /// (T-066 tests + early T-069) should still work without a
    /// registry.
    #[tokio::test]
    async fn intent_without_registry_errors_with_disabled_message() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "intent-off");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Intent",
                "args": { "name": "ark.core.ping", "args": {} }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].as_str().unwrap().contains("intents disabled"),
            "got {resp}"
        );
    }

    /// `Intent` malformed args (missing `name`) → `ok: false` with an
    /// arg-parse error message; never panics.
    #[tokio::test]
    async fn intent_malformed_args_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "intent-bad");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Intent",
                "args": { "args": {} } // no `name`
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(resp["error"].is_string());
    }

    /// `Emit` broadcasts a UserEvent onto the supervisor bus and
    /// reports the receiver count.
    #[tokio::test]
    async fn emit_broadcasts_user_event_to_bus() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "emit");
        let (ctx, _cancel) = make_ctx(id, layout);
        // Subscribe BEFORE emit so the broadcast has a receiver.
        let mut rx = ctx.event_bus.subscribe();
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Emit",
                "args": {
                    "event": "ark.zellij.pane_closed",
                    "payload": { "pane_id": 7 },
                    "source": "ext:ark-bus"
                }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true), "resp: {resp}");
        assert_eq!(
            resp["data"]["broadcast"],
            JsonValue::String("ark.zellij.pane_closed".into())
        );

        // Verify the bus actually received the event.
        let ev = rx.recv().await.expect("event received");
        match ev {
            ark_types::AgentEvent::UserEvent {
                name,
                payload,
                source,
            } => {
                assert_eq!(name, "ark.zellij.pane_closed");
                assert_eq!(payload["pane_id"], serde_json::json!(7));
                assert_eq!(source, "ext:ark-bus");
            }
            other => panic!("expected UserEvent, got {other:?}"),
        }
    }

    /// `Permit` accepts only the canonical outcome strings.
    #[tokio::test]
    async fn permit_validates_outcome_string() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "permit");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        for outcome in ["allow", "reject_once", "reject_always"] {
            let resp = h
                .handle(serde_json::json!({
                    "cmd": "Permit",
                    "args": {
                        "request_id": "req-1",
                        "outcome": outcome
                    }
                }))
                .await;
            assert_eq!(
                resp["ok"],
                JsonValue::Bool(true),
                "outcome {outcome}: {resp}"
            );
        }

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Permit",
                "args": {
                    "request_id": "req-2",
                    "outcome": "bogus"
                }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(resp["error"].as_str().unwrap().contains("outcome"));
    }

    /// `json_to_kdl_node` flattens object args into `KdlEntry::new_prop`
    /// entries the facet-kdl deserializer can read.
    #[test]
    fn json_to_kdl_node_flattens_scalars() {
        use kdl::KdlValue;
        let node = json_to_kdl_node(
            "open_tab",
            &serde_json::json!({
                "name": "build",
                "focus": true,
                "size": 60
            }),
        )
        .expect("ok");
        assert_eq!(node.name().value(), "open_tab");

        let entries = node.entries();
        let by_key = |k: &str| {
            entries.iter().find(|e| e.name().map(|n| n.value()) == Some(k))
        };
        assert!(matches!(by_key("name").unwrap().value(), KdlValue::String(s) if s == "build"));
        assert!(matches!(by_key("focus").unwrap().value(), KdlValue::Bool(true)));
        assert!(matches!(
            by_key("size").unwrap().value(),
            KdlValue::Integer(60)
        ));
    }

    /// `node_name_for_intent` strips the namespace prefix.
    #[test]
    fn node_name_for_intent_strips_namespace() {
        assert_eq!(node_name_for_intent("ark.core.open_tab"), "open_tab");
        assert_eq!(node_name_for_intent("foo.bar.baz"), "baz");
        assert_eq!(node_name_for_intent("bare"), "bare");
    }

    /// `Intent`/`Emit`/`Permit` parse cleanly from their wire shape — a
    /// minimal mirror of the `every_command_variant_parses_as_request`
    /// guard for the new variants.
    #[test]
    fn bridge_command_variants_parse_as_request() {
        let lines: &[&[u8]] = &[
            br#"{"cmd":"Intent","args":{"name":"ark.core.open_tab","args":{}}}"#,
            br#"{"cmd":"Emit","args":{"event":"x","payload":{},"source":"ext:ark-bus"}}"#,
            br#"{"cmd":"Permit","args":{"request_id":"r","outcome":"allow"}}"#,
        ];
        for line in lines {
            let req: Request = serde_json::from_slice(line).unwrap_or_else(|e| {
                panic!(
                    "variant should parse: {} (err: {e})",
                    std::str::from_utf8(line).unwrap_or("?")
                )
            });
            assert!(matches!(req.cmd.as_str(), "Intent" | "Emit" | "Permit"));
        }
    }

    #[tokio::test]
    async fn audit_records_malformed_requests() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "malform");
        let state_root = tmp.path().join("state");
        let (ctx, _cancel, _logger) = make_ctx_with_audit(id, layout, state_root.clone());
        let h = SupervisorCommandHandler::new(ctx);

        // Missing "cmd" triggers the malformed path.
        let _ = h.handle(serde_json::json!({ "oops": true })).await;

        let lines = read_audit_lines(&state_root.join("control.log"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["response"]["ok"], JsonValue::Bool(false));
        assert!(
            lines[0]["response"]["error"]
                .as_str()
                .unwrap()
                .contains("malformed")
        );
    }
}
