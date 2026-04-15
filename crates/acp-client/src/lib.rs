//! ark ACP client — wraps the `agent-client-protocol` crate.
//!
//! ark is a first-class ACP client (cavekit-scene R17). Engines
//! (`claude`, `codex`, `gemini`, …) are ACP **agents**; ark spawns
//! them as subprocesses, speaks JSON-RPC over the engine's stdio, and
//! translates every inbound `session/update` notification plus every
//! `session/request_permission`, `fs/*`, `terminal/*` request into
//! ark's internal [`AgentEvent::UserEvent`] taxonomy. Each emitted
//! event carries the namespaced name (`ark.acp.<kind>`), the raw
//! payload, and the `source = "core"` provenance tag expected by
//! `ark scene explain` (cavekit-scene R4).
//!
//! # Architecture
//!
//! The upstream [`agent_client_protocol::ClientSideConnection`] is
//! built around [`agent_client_protocol::Client`], which is `?Send`
//! and `?Sync` — the connection's I/O driver uses
//! `futures::future::LocalBoxFuture` + `spawn_local`. That forces the
//! entire ACP state machine onto a single thread.
//!
//! To give supervisor callers a normal `Send + Sync` façade, the
//! [`AcpClient`] confines all ACP work to a dedicated OS thread
//! running a `tokio::task::LocalSet`. The public async API is a set
//! of tiny senders that:
//!
//! 1. Post a typed [`AcpCommand`] onto a `tokio::sync::mpsc` queue.
//! 2. Await the matching `oneshot::Receiver` reply.
//!
//! The I/O thread spawns the ACP connection (via
//! [`ClientSideConnection::new`]), calls `initialize` + `new_session`,
//! and routes incoming session updates + callback requests into the
//! [`AgentEvent`] stream ([`events`](AcpClient::events)).
//!
//! # Event taxonomy
//!
//! Every translated [`AgentEvent::UserEvent`] carries
//! `source = "core"`. Names:
//!
//! | ACP surface                          | `name`                          |
//! |--------------------------------------|---------------------------------|
//! | `session/update::Plan`                | `ark.acp.plan`                  |
//! | `session/update::AgentMessageChunk`   | `ark.acp.agent_message_chunk`   |
//! | `session/update::AgentThoughtChunk`   | `ark.acp.agent_thought_chunk`   |
//! | `session/update::UserMessageChunk`    | `ark.acp.user_message_chunk`    |
//! | `session/update::ToolCall`            | `ark.acp.tool_call`             |
//! | `session/update::ToolCallUpdate`      | `ark.acp.tool_call_update`      |
//! | `session/update::CurrentModeUpdate`   | `ark.acp.current_mode_update`   |
//! | `session/update::*` (other)           | `ark.acp.session_update` (generic bucket) |
//! | `session/request_permission`          | `ark.acp.permission_requested`  |
//! | `fs/read_text_file`                   | `ark.acp.fs.read`               |
//! | `fs/write_text_file`                  | `ark.acp.fs.write`              |
//! | `terminal/create`                     | `ark.acp.terminal.create`       |
//! | `terminal/output`                     | `ark.acp.terminal.output`       |
//! | `terminal/release`                    | `ark.acp.terminal.release`      |
//! | `terminal/wait_for_exit`              | `ark.acp.terminal.wait_for_exit`|
//! | `terminal/kill`                       | `ark.acp.terminal.kill`         |
//!
//! See [`event_names`] for the constants.
//!
//! # Status
//!
//! T-ACP.2 produces the standalone crate. T-ACP.4a/4b wire it into
//! the supervisor (per-agent runtime + turn-inflight tracking). This
//! crate is intentionally decoupled from supervisor/scene so the wire
//! pieces stay swappable.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_client_protocol::{
    Agent as _, CancelNotification, Client as AcpClientTrait, ClientSideConnection, ContentBlock,
    CreateTerminalRequest, CreateTerminalResponse, ExtNotification, ExtRequest, ExtResponse,
    InitializeRequest, KillTerminalRequest, KillTerminalResponse, NewSessionRequest,
    PermissionOptionId, PromptRequest, ProtocolVersion, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionModeId, SessionNotification, SessionUpdate,
    SetSessionModeRequest, TerminalOutputRequest, TerminalOutputResponse, TextContent,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use ark_types::event::AgentEvent;
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Lowered engine-launch spec used to spawn the subprocess.
///
/// Mirrors `ark_scene::engine::EngineLaunch` shape. Duplicated here
/// as a tiny value type so this crate can stay dep-clean — scene
/// depends on acp-client (via the T-ACP.2b ops wiring), not the
/// other way round. The supervisor converts the scene type into
/// this value when calling [`AcpClient::spawn`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineLaunch {
    /// Human-friendly engine identifier (e.g. `"claude"`).
    pub name: String,
    /// Executable path or argv-0 used to spawn the agent.
    pub command: String,
    /// Additional positional arguments.
    pub args: Vec<String>,
    /// Extra environment variables merged over the parent env.
    pub env: std::collections::BTreeMap<String, String>,
    /// Working directory the agent subprocess inherits. Defaults to
    /// the parent cwd when `None`.
    pub cwd: Option<std::path::PathBuf>,
}

/// Fatal-ish error surface for the ACP client.
///
/// `AcpError::Protocol` carries anything bubbling out of the
/// underlying [`agent_client_protocol::Error`]; `AcpError::Runtime`
/// captures supervisor-glue failures (subprocess spawn, channel
/// closed, background task panic).
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The ACP worker thread exited or the command channel is closed.
    #[error("acp client is not running")]
    NotRunning,

    /// Failed to spawn the engine subprocess.
    #[error("failed to spawn engine `{command}`: {source}")]
    Spawn {
        /// Command we tried to spawn.
        command: String,
        /// OS error from `tokio::process::Command::spawn`.
        #[source]
        source: std::io::Error,
    },

    /// Underlying ACP protocol error (JSON-RPC error, method not
    /// found, capability violation, …).
    #[error("acp protocol error: {0}")]
    Protocol(String),

    /// Timed out waiting for an ACP response.
    #[error("acp operation timed out: {0}")]
    Timeout(String),

    /// Unknown permission request id supplied to
    /// [`AcpClient::permit`].
    #[error("unknown permission request id `{0}`")]
    UnknownPermissionRequest(String),

    /// Miscellaneous runtime errors.
    #[error("acp runtime error: {0}")]
    Runtime(String),
}

impl From<agent_client_protocol::Error> for AcpError {
    fn from(err: agent_client_protocol::Error) -> Self {
        AcpError::Protocol(err.to_string())
    }
}

/// User's decision on a pending permission request, surfaced by
/// `session/request_permission` and returned via [`AcpClient::permit`].
///
/// Mirrors [`RequestPermissionOutcome`] in the wire protocol, shrunk
/// down to the minimum the supervisor / scene layer needs. The
/// selected-option path threads through the `option_id` the scene
/// picked (scenes enumerate options via the `ark.acp.permission_requested`
/// payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermitOutcome {
    /// The user explicitly selected an option by `option_id`. Maps to
    /// [`RequestPermissionOutcome::Selected`].
    Selected {
        /// `option_id` the user picked. Must match one of the options
        /// the agent advertised in the `ark.acp.permission_requested`
        /// event's `options` array.
        option_id: String,
    },
    /// The prompt was cancelled before the user responded. Maps to
    /// [`RequestPermissionOutcome::Cancelled`].
    Cancelled,
}

impl PermitOutcome {
    fn into_response(self) -> RequestPermissionResponse {
        let outcome = match self {
            PermitOutcome::Selected { option_id } => {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    PermissionOptionId::from(option_id),
                ))
            }
            PermitOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
        };
        RequestPermissionResponse::new(outcome)
    }
}

/// Handle returned from [`AcpClient::prompt`] — the `session_id` +
/// a channel the caller can `.await` for the final [`StopReason`].
///
/// Returned early (as soon as `session/prompt` dispatch has been
/// accepted) so the caller can observe the streamed
/// `agent_message_chunk` / `tool_call` / `tool_call_update` events
/// without being blocked on turn completion. The `stop` future
/// resolves when the matching `session/prompt` response comes back.
///
/// [`StopReason`]: agent_client_protocol::StopReason
#[derive(Debug)]
pub struct PromptHandle {
    /// Monotonic-within-process id the turn-inflight tracker uses as
    /// its JSON-RPC correlation key. Not a real JSON-RPC id (the
    /// upstream crate owns those); shaped as an opaque string so
    /// T-ACP.2c's `TurnInflightTracker` doesn't leak wire details.
    pub jsonrpc_id: String,
    /// Session this prompt was dispatched into.
    pub session_id: String,
    /// Resolves when the `session/prompt` response returns, with
    /// whatever [`StopReason`] the agent reported.
    ///
    /// [`StopReason`]: agent_client_protocol::StopReason
    pub stop: oneshot::Receiver<Result<agent_client_protocol::StopReason, AcpError>>,
}

// ---------------------------------------------------------------------------
// event names (public constants)
// ---------------------------------------------------------------------------

/// Public constants for every translated `UserEvent::name`.
///
/// Scene authors match selectors of the form `"UserEvent:<name>"`; these
/// constants guarantee the spelling. Changing any string here is a
/// breaking wire change for user scenes.
pub mod event_names {
    /// `session/update::Plan` — the agent's execution plan.
    pub const PLAN: &str = "ark.acp.plan";
    /// `session/update::AgentMessageChunk` — streamed model output.
    pub const AGENT_MESSAGE_CHUNK: &str = "ark.acp.agent_message_chunk";
    /// `session/update::AgentThoughtChunk` — streamed internal reasoning.
    pub const AGENT_THOUGHT_CHUNK: &str = "ark.acp.agent_thought_chunk";
    /// `session/update::UserMessageChunk` — streamed user echo.
    pub const USER_MESSAGE_CHUNK: &str = "ark.acp.user_message_chunk";
    /// `session/update::ToolCall` — a new tool-call request from the agent.
    pub const TOOL_CALL: &str = "ark.acp.tool_call";
    /// `session/update::ToolCallUpdate` — status/output update on a tool call.
    pub const TOOL_CALL_UPDATE: &str = "ark.acp.tool_call_update";
    /// `session/update::CurrentModeUpdate` — session mode changed.
    pub const CURRENT_MODE_UPDATE: &str = "ark.acp.current_mode_update";
    /// `session/update::*` (unknown / non-enumerated variants).
    pub const SESSION_UPDATE: &str = "ark.acp.session_update";
    /// `session/request_permission` — agent asked for a tool-call authorisation.
    pub const PERMISSION_REQUESTED: &str = "ark.acp.permission_requested";
    /// `fs/read_text_file` — agent asked ark to read a file.
    pub const FS_READ: &str = "ark.acp.fs.read";
    /// `fs/write_text_file` — agent asked ark to write a file.
    pub const FS_WRITE: &str = "ark.acp.fs.write";
    /// `terminal/create` — agent spawned a managed terminal.
    pub const TERMINAL_CREATE: &str = "ark.acp.terminal.create";
    /// `terminal/output` — agent polled a managed terminal's output.
    pub const TERMINAL_OUTPUT: &str = "ark.acp.terminal.output";
    /// `terminal/release` — agent released a managed terminal.
    pub const TERMINAL_RELEASE: &str = "ark.acp.terminal.release";
    /// `terminal/wait_for_exit` — agent waited for a terminal to exit.
    pub const TERMINAL_WAIT_FOR_EXIT: &str = "ark.acp.terminal.wait_for_exit";
    /// `terminal/kill` — agent killed a managed terminal.
    pub const TERMINAL_KILL: &str = "ark.acp.terminal.kill";
}

// ---------------------------------------------------------------------------
// AcpCommand — cross-thread message type onto the LocalSet worker
// ---------------------------------------------------------------------------

/// Internal command queued onto the ACP worker thread. Not public.
///
/// The public [`AcpClient`] methods package each call into one of
/// these, hand it over the `mpsc`, and await the matching
/// `oneshot::Receiver`. This indirection exists because the upstream
/// [`ClientSideConnection`] is `?Send`, so we can never hold it
/// across an `.await` point on any other thread.
enum AcpCommand {
    Prompt {
        text: String,
        handle_tx: oneshot::Sender<Result<PromptHandle, AcpError>>,
    },
    Cancel {
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Permit {
        request_id: String,
        outcome: PermitOutcome,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    SetMode {
        mode: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

// ---------------------------------------------------------------------------
// AcpClient — public façade
// ---------------------------------------------------------------------------

/// ark's ACP client — cross-thread façade over
/// [`agent_client_protocol::ClientSideConnection`].
///
/// Spawns the engine subprocess + the ACP I/O state machine on a
/// dedicated OS thread that runs a `tokio::task::LocalSet`.
/// Cross-thread callers (supervisor, scene ops) interact through
/// `Send + Sync`-safe methods that post commands onto an `mpsc` queue
/// and await `oneshot` replies.
///
/// Dropping [`AcpClient`] sends a [`AcpCommand::Shutdown`] and joins
/// the worker thread; the engine subprocess is killed via
/// `tokio::process::Child::kill` in the worker's cleanup path.
#[derive(Debug)]
pub struct AcpClient {
    /// Sender onto the worker thread's `mpsc`. Cloning is cheap.
    cmd_tx: mpsc::Sender<AcpCommand>,
    /// Event stream fan-out — every translated [`AgentEvent::UserEvent`]
    /// hits every receiver created with [`events`](Self::events).
    event_tx: broadcast::Sender<AgentEvent>,
    /// Thread join handle, consumed by `drop`.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AcpClient {
    /// Spawn the engine subprocess defined by `launch` and drive the
    /// ACP handshake (`initialize` + `new_session`) on a dedicated
    /// I/O thread. Returns once the handshake completes.
    ///
    /// # Errors
    ///
    /// Fails if the subprocess couldn't be spawned or the ACP
    /// handshake errored.
    pub async fn spawn(launch: EngineLaunch) -> Result<Self, AcpError> {
        let (event_tx, _event_rx) = broadcast::channel::<AgentEvent>(256);
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCommand>(64);
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), AcpError>>();

        let event_tx_worker = event_tx.clone();
        let launch_worker = launch.clone();
        let worker = std::thread::Builder::new()
            .name(format!("acp-client:{}", launch.name))
            .spawn(move || {
                // Each worker thread owns its own single-threaded
                // tokio runtime. A multi-thread runtime would not
                // buy us anything here because the ACP surface is
                // `?Send` — we cannot migrate futures across cores.
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(AcpError::Runtime(format!(
                            "tokio runtime build failed: {e}"
                        ))));
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(
                    &rt,
                    acp_worker_main(launch_worker, cmd_rx, event_tx_worker, ready_tx),
                );
            })
            .map_err(|e| AcpError::Runtime(format!("worker thread spawn failed: {e}")))?;

        // Await handshake completion. If the worker panics before
        // replying, `ready_rx` errors with `RecvError` → Runtime.
        match ready_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(AcpError::Runtime(
                    "worker thread exited before ACP handshake".into(),
                ));
            }
        }

        Ok(Self {
            cmd_tx,
            event_tx,
            worker: Some(worker),
        })
    }

    /// Send a `session/prompt` with a single [`TextContent`] block
    /// carrying `prompt_text`.
    ///
    /// Returns a [`PromptHandle`] whose `stop` receiver resolves when
    /// the agent returns its `StopReason`.
    pub async fn prompt(&self, prompt_text: &str) -> Result<PromptHandle, AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Prompt {
                text: prompt_text.to_string(),
                handle_tx: tx,
            })
            .await
            .map_err(|_| AcpError::NotRunning)?;
        rx.await.map_err(|_| AcpError::NotRunning)?
    }

    /// Send a `session/cancel` notification. The agent is expected
    /// to wind down in-flight tool calls and respond to the open
    /// `session/prompt` with [`StopReason::Cancelled`].
    ///
    /// [`StopReason::Cancelled`]: agent_client_protocol::StopReason::Cancelled
    pub async fn cancel(&self) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Cancel { reply: tx })
            .await
            .map_err(|_| AcpError::NotRunning)?;
        rx.await.map_err(|_| AcpError::NotRunning)?
    }

    /// Respond to an open `session/request_permission` identified by
    /// `request_id` (the value surfaced on the
    /// `ark.acp.permission_requested` event).
    pub async fn permit(
        &self,
        request_id: &str,
        outcome: PermitOutcome,
    ) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Permit {
                request_id: request_id.to_string(),
                outcome,
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotRunning)?;
        rx.await.map_err(|_| AcpError::NotRunning)?
    }

    /// Send a `session/set_mode` request.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::SetMode {
                mode: mode.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotRunning)?;
        rx.await.map_err(|_| AcpError::NotRunning)?
    }

    /// Subscribe to the translated [`AgentEvent::UserEvent`] stream.
    ///
    /// Every receiver gets every event from the moment of subscription
    /// forward. Backpressure: the underlying channel is bounded to
    /// 256 events; slow consumers lag — they receive a
    /// [`broadcast::error::RecvError::Lagged`] on the next `recv`,
    /// after which they resume from the current tail.
    pub fn events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // Fire-and-forget shutdown.  If the channel is closed (worker
        // already gone) there is nothing to wait on.
        let (tx, _rx) = oneshot::channel();
        let _ = self.cmd_tx.try_send(AcpCommand::Shutdown { reply: tx });
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker thread — owns the ClientSideConnection, runs the LocalSet
// ---------------------------------------------------------------------------

/// Internal state the worker thread holds across command dispatches.
struct SessionState {
    /// ID returned by `session/new`.
    session_id: SessionId,
    /// Pending `session/request_permission` requests the client must
    /// respond to; keyed by the `request_id` surfaced on the
    /// `ark.acp.permission_requested` event.
    pending_permissions: HashMap<String, oneshot::Sender<RequestPermissionResponse>>,
}

/// Main worker body — owns the ACP connection and the engine child.
async fn acp_worker_main(
    launch: EngineLaunch,
    mut cmd_rx: mpsc::Receiver<AcpCommand>,
    event_tx: broadcast::Sender<AgentEvent>,
    ready_tx: oneshot::Sender<Result<(), AcpError>>,
) {
    // --- 1. spawn the subprocess -------------------------------------
    let spawn_result = spawn_engine(&launch);
    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            let _ = ready_tx.send(Err(AcpError::Runtime(
                "engine subprocess has no stdin".into(),
            )));
            let _ = child.kill().await;
            return;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = ready_tx.send(Err(AcpError::Runtime(
                "engine subprocess has no stdout".into(),
            )));
            let _ = child.kill().await;
            return;
        }
    };

    // The ACP crate wants `futures::AsyncRead`/`AsyncWrite`. tokio's
    // `ChildStdin`/`ChildStdout` implement `tokio::io::{AsyncRead,
    // AsyncWrite}`; we adapt with tokio-util's compat layer. That's
    // the only reason we need `tokio-util` — if a future release of
    // the ACP crate takes `tokio::io::*` directly we drop this hop.
    //
    // TODO(post-v1): drop the tokio-util compat hop if
    // agent-client-protocol grows tokio-native I/O ctors.
    let outgoing = stdin.compat_write();
    let incoming = stdout.compat();

    // --- 2. build the ACP connection --------------------------------
    let handler_shared: Arc<PendingPermissions> = Arc::new(PendingPermissions::default());
    let handler = ClientHandler {
        event_tx: event_tx.clone(),
        pending: Arc::clone(&handler_shared),
    };

    let (conn, io_task) = ClientSideConnection::new(
        handler,
        outgoing,
        incoming,
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );

    // io_task drives the framing layer — keep it alive for the worker
    // lifetime, but it MUST be `spawn_local`-ed (not `spawn`-ed) because
    // the ACP crate uses `futures::future::LocalBoxFuture`.
    let io_handle = tokio::task::spawn_local(async move {
        let _ = io_task.await;
    });

    // --- 3. handshake: initialize + new_session ----------------------
    let init = InitializeRequest::new(ProtocolVersion::LATEST);
    let init_result = conn.initialize(init).await;
    if let Err(e) = init_result {
        let _ = ready_tx.send(Err(AcpError::Protocol(format!(
            "initialize failed: {e}"
        ))));
        let _ = child.kill().await;
        io_handle.abort();
        return;
    }

    let cwd = launch
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")));
    let new_session_req = NewSessionRequest::new(cwd);
    let session_id = match conn.new_session(new_session_req).await {
        Ok(resp) => resp.session_id,
        Err(e) => {
            let _ = ready_tx.send(Err(AcpError::Protocol(format!(
                "new_session failed: {e}"
            ))));
            let _ = child.kill().await;
            io_handle.abort();
            return;
        }
    };

    // Handshake green — unblock the caller of `AcpClient::spawn`.
    let _ = ready_tx.send(Ok(()));

    // `Rc<ClientSideConnection>` so we can `spawn_local` per-request
    // tasks and still have the command loop hold its own reference.
    // Cross-request parallelism (cancel landing during an in-flight
    // prompt) is only possible because the tasks share this Rc; a
    // single owned `conn` held by the command-loop future would
    // serialize them.
    let conn = std::rc::Rc::new(conn);
    let state = SessionState {
        session_id: session_id.clone(),
        pending_permissions: HashMap::new(),
    };
    let state = std::rc::Rc::new(std::cell::RefCell::new(state));
    let jsonrpc_ctr = Arc::new(AtomicU64::new(0));

    // --- 4. command loop --------------------------------------------
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            AcpCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
            AcpCommand::Prompt { text, handle_tx } => {
                let conn = std::rc::Rc::clone(&conn);
                let state = std::rc::Rc::clone(&state);
                let ctr = Arc::clone(&jsonrpc_ctr);
                // `spawn_local` so prompt + cancel + permit can
                // interleave. Cancel uses a fire-and-forget
                // notification so it doesn't block waiting for the
                // prompt future.
                tokio::task::spawn_local(async move {
                    handle_prompt(&conn, &state, &ctr, text, handle_tx).await;
                });
            }
            AcpCommand::Cancel { reply } => {
                let session_id = state.borrow().session_id.clone();
                let r = conn
                    .cancel(CancelNotification::new(session_id))
                    .await
                    .map_err(AcpError::from);
                let _ = reply.send(r);
            }
            AcpCommand::Permit {
                request_id,
                outcome,
                reply,
            } => {
                let r = handler_shared.respond(&request_id, outcome.into_response());
                match r {
                    Ok(()) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
                state.borrow_mut().pending_permissions.remove(&request_id);
            }
            AcpCommand::SetMode { mode, reply } => {
                let session_id = state.borrow().session_id.clone();
                let req = SetSessionModeRequest::new(session_id, SessionModeId::from(mode));
                let r = conn
                    .set_session_mode(req)
                    .await
                    .map(|_| ())
                    .map_err(AcpError::from);
                let _ = reply.send(r);
            }
        }
    }

    // Clean exit — kill the subprocess, abort the I/O task.
    let _ = child.kill().await;
    io_handle.abort();
}

/// Dispatch a `session/prompt` call + hand the caller a
/// [`PromptHandle`] immediately. Runs inside a `spawn_local` task so
/// cancel/permit/set-mode commands can land during the turn.
async fn handle_prompt(
    conn: &ClientSideConnection,
    state: &std::rc::Rc<std::cell::RefCell<SessionState>>,
    ctr: &AtomicU64,
    text: String,
    handle_tx: oneshot::Sender<Result<PromptHandle, AcpError>>,
) {
    let n = ctr.fetch_add(1, Ordering::SeqCst);
    let jsonrpc_id = format!("prompt-{n}");
    let (stop_tx, stop_rx) = oneshot::channel();

    let session_id = state.borrow().session_id.clone();
    let handle = PromptHandle {
        jsonrpc_id: jsonrpc_id.clone(),
        session_id: session_id.0.to_string(),
        stop: stop_rx,
    };
    // Surface the handle to the caller first so they can subscribe
    // to events before the first chunk arrives.
    let _ = handle_tx.send(Ok(handle));

    let req = PromptRequest::new(
        session_id,
        vec![ContentBlock::Text(TextContent::new(text))],
    );
    let resp = conn.prompt(req).await;
    let out = match resp {
        Ok(r) => Ok(r.stop_reason),
        Err(e) => Err(AcpError::from(e)),
    };
    let _ = stop_tx.send(out);
}

// ---------------------------------------------------------------------------
// Subprocess spawn
// ---------------------------------------------------------------------------

fn spawn_engine(launch: &EngineLaunch) -> Result<Child, AcpError> {
    if launch.command.is_empty() {
        return Err(AcpError::Runtime(
            "engine launch spec has empty `command`".into(),
        ));
    }
    let mut cmd = Command::new(&launch.command);
    cmd.args(&launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &launch.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &launch.cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().map_err(|e| AcpError::Spawn {
        command: launch.command.clone(),
        source: e,
    })
}

// ---------------------------------------------------------------------------
// PendingPermissions — shared state between the worker loop and the
// inbound-request handler.
// ---------------------------------------------------------------------------

/// Map from `request_id` → `oneshot::Sender<RequestPermissionResponse>`.
///
/// The inbound `ClientHandler::request_permission` callback stores the
/// oneshot sender here, emits the `ark.acp.permission_requested` event,
/// and awaits the corresponding receiver. The worker loop's
/// [`AcpCommand::Permit`] branch resolves the sender with the scene's
/// decision.
#[derive(Default, Debug)]
struct PendingPermissions {
    inner: std::sync::Mutex<HashMap<String, oneshot::Sender<RequestPermissionResponse>>>,
    next_id: AtomicU64,
}

impl PendingPermissions {
    fn insert(&self, tx: oneshot::Sender<RequestPermissionResponse>) -> String {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let key = format!("perm-{id}");
        let mut guard = self.inner.lock().expect("permissions mutex poisoned");
        guard.insert(key.clone(), tx);
        key
    }

    fn respond(
        &self,
        request_id: &str,
        response: RequestPermissionResponse,
    ) -> Result<(), AcpError> {
        let mut guard = self.inner.lock().expect("permissions mutex poisoned");
        match guard.remove(request_id) {
            Some(sender) => {
                sender
                    .send(response)
                    .map_err(|_| AcpError::Runtime("permission receiver dropped".into()))?;
                Ok(())
            }
            None => Err(AcpError::UnknownPermissionRequest(request_id.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// ClientHandler — impls agent-client-protocol::Client
// ---------------------------------------------------------------------------

/// Inbound-handler side: receives every notification/request the
/// agent sends *to* us, translates each into an
/// [`AgentEvent::UserEvent`] on the broadcast bus.
///
/// Because the upstream [`AcpClientTrait`] is `?Send`, `ClientHandler`
/// only ever exists on the worker thread — its `event_tx` and `pending`
/// fields are `Send + Sync` specifically so we can ship replies
/// back (cross-thread `permit` responses) without threading a
/// non-`Send` handle through the supervisor.
struct ClientHandler {
    event_tx: broadcast::Sender<AgentEvent>,
    pending: Arc<PendingPermissions>,
}

#[async_trait::async_trait(?Send)]
impl AcpClientTrait for ClientHandler {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> Result<RequestPermissionResponse, agent_client_protocol::Error> {
        let (tx, rx) = oneshot::channel();
        let request_id = self.pending.insert(tx);

        // Serialize the inbound args once so the event payload
        // mirrors the wire shape verbatim (plus the request_id we
        // minted ourselves).
        let mut payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(ref mut m) = payload {
            m.insert(
                "request_id".into(),
                serde_json::Value::String(request_id.clone()),
            );
        }
        emit_event(&self.event_tx, event_names::PERMISSION_REQUESTED, payload);

        match rx.await {
            Ok(resp) => Ok(resp),
            Err(_) => Err(agent_client_protocol::Error::internal_error()),
        }
    }

    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> Result<(), agent_client_protocol::Error> {
        let name = match &args.update {
            SessionUpdate::Plan(_) => event_names::PLAN,
            SessionUpdate::AgentMessageChunk(_) => event_names::AGENT_MESSAGE_CHUNK,
            SessionUpdate::AgentThoughtChunk(_) => event_names::AGENT_THOUGHT_CHUNK,
            SessionUpdate::UserMessageChunk(_) => event_names::USER_MESSAGE_CHUNK,
            SessionUpdate::ToolCall(_) => event_names::TOOL_CALL,
            SessionUpdate::ToolCallUpdate(_) => event_names::TOOL_CALL_UPDATE,
            SessionUpdate::CurrentModeUpdate(_) => event_names::CURRENT_MODE_UPDATE,
            _ => event_names::SESSION_UPDATE,
        };
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, name, payload);
        Ok(())
    }

    async fn read_text_file(
        &self,
        args: ReadTextFileRequest,
    ) -> Result<ReadTextFileResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::FS_READ, payload);
        // TODO(T-ACP.4a): route through ark_supervisor's permission-
        // gated FS surface. For now fall through to method_not_found
        // so the agent fails closed.
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn write_text_file(
        &self,
        args: WriteTextFileRequest,
    ) -> Result<WriteTextFileResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::FS_WRITE, payload);
        // TODO(T-ACP.4a): wire to permission-gated FS surface.
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn create_terminal(
        &self,
        args: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::TERMINAL_CREATE, payload);
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn terminal_output(
        &self,
        args: TerminalOutputRequest,
    ) -> Result<TerminalOutputResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::TERMINAL_OUTPUT, payload);
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn release_terminal(
        &self,
        args: ReleaseTerminalRequest,
    ) -> Result<ReleaseTerminalResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::TERMINAL_RELEASE, payload);
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn wait_for_terminal_exit(
        &self,
        args: WaitForTerminalExitRequest,
    ) -> Result<WaitForTerminalExitResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::TERMINAL_WAIT_FOR_EXIT, payload);
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn kill_terminal(
        &self,
        args: KillTerminalRequest,
    ) -> Result<KillTerminalResponse, agent_client_protocol::Error> {
        let payload = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        emit_event(&self.event_tx, event_names::TERMINAL_KILL, payload);
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn ext_method(
        &self,
        _args: ExtRequest,
    ) -> Result<ExtResponse, agent_client_protocol::Error> {
        Err(agent_client_protocol::Error::method_not_found())
    }

    async fn ext_notification(
        &self,
        _args: ExtNotification,
    ) -> Result<(), agent_client_protocol::Error> {
        Ok(())
    }
}

/// Push a `UserEvent` onto the broadcast bus. Receiver count is zero
/// when no scene has subscribed yet — we drop silently in that case
/// rather than stalling the ACP state machine.
fn emit_event(event_tx: &broadcast::Sender<AgentEvent>, name: &str, payload: serde_json::Value) {
    let event = AgentEvent::UserEvent {
        name: name.to_string(),
        payload,
        source: "core".to_string(),
    };
    let _ = event_tx.send(event);
    tracing::trace!(
        target = "acp_client::events",
        event = name,
        "translated ACP notification → ark.acp.*"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test — every R17-surface constant is spelled as
    /// `ark.acp.*` and matches what scene selectors expect.
    #[test]
    fn every_event_name_is_namespaced() {
        for name in [
            event_names::PLAN,
            event_names::AGENT_MESSAGE_CHUNK,
            event_names::AGENT_THOUGHT_CHUNK,
            event_names::USER_MESSAGE_CHUNK,
            event_names::TOOL_CALL,
            event_names::TOOL_CALL_UPDATE,
            event_names::CURRENT_MODE_UPDATE,
            event_names::SESSION_UPDATE,
            event_names::PERMISSION_REQUESTED,
            event_names::FS_READ,
            event_names::FS_WRITE,
            event_names::TERMINAL_CREATE,
            event_names::TERMINAL_OUTPUT,
            event_names::TERMINAL_RELEASE,
            event_names::TERMINAL_WAIT_FOR_EXIT,
            event_names::TERMINAL_KILL,
        ] {
            assert!(name.starts_with("ark.acp."), "{name}");
        }
    }

    #[test]
    fn permit_outcome_selected_round_trip() {
        let o = PermitOutcome::Selected {
            option_id: "allow-once".into(),
        };
        let resp = o.into_response();
        match resp.outcome {
            RequestPermissionOutcome::Selected(sel) => {
                assert_eq!(sel.option_id.0.as_ref(), "allow-once");
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn permit_outcome_cancelled_round_trip() {
        let o = PermitOutcome::Cancelled;
        let resp = o.into_response();
        assert!(matches!(
            resp.outcome,
            RequestPermissionOutcome::Cancelled
        ));
    }

    /// Invoking `permit` with an unknown id yields
    /// `UnknownPermissionRequest`.
    #[test]
    fn pending_permissions_reject_unknown_id() {
        let p = PendingPermissions::default();
        let err = p
            .respond(
                "does-not-exist",
                PermitOutcome::Cancelled.into_response(),
            )
            .unwrap_err();
        match err {
            AcpError::UnknownPermissionRequest(id) => assert_eq!(id, "does-not-exist"),
            other => panic!("expected UnknownPermissionRequest, got {other:?}"),
        }
    }

    /// A registered request id can be responded to exactly once.
    #[tokio::test]
    async fn pending_permissions_single_response() {
        let p = PendingPermissions::default();
        let (tx, rx) = oneshot::channel();
        let id = p.insert(tx);
        p.respond(&id, PermitOutcome::Cancelled.into_response())
            .expect("first respond");
        let got = rx.await.expect("receiver got value");
        assert!(matches!(
            got.outcome,
            RequestPermissionOutcome::Cancelled
        ));
        // Second respond fails — id already consumed.
        let err = p
            .respond(&id, PermitOutcome::Cancelled.into_response())
            .unwrap_err();
        assert!(matches!(err, AcpError::UnknownPermissionRequest(_)));
    }

    /// Spawning with an empty command errors out eagerly rather
    /// than silently forking an empty child.
    #[tokio::test]
    async fn spawn_rejects_empty_command() {
        let launch = EngineLaunch::default();
        let err = AcpClient::spawn(launch).await.unwrap_err();
        match err {
            AcpError::Runtime(msg) => assert!(msg.contains("empty")),
            other => panic!("expected Runtime, got {other:?}"),
        }
    }

    /// Feed a [`SessionNotification`] directly at the translation
    /// layer and assert the published [`AgentEvent::UserEvent`]
    /// carries the correct `name` + `source`. Exercises every
    /// spec-surface [`SessionUpdate`] variant the R17 translation
    /// table enumerates (plus the generic bucket).
    #[tokio::test(flavor = "current_thread")]
    async fn session_notification_translation_table() {
        use agent_client_protocol::{
            ContentChunk, CurrentModeUpdate, Plan, PlanEntry, PlanEntryPriority,
            PlanEntryStatus, ToolCall, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
        };

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let (event_tx, mut event_rx) = broadcast::channel::<AgentEvent>(64);
                let handler = ClientHandler {
                    event_tx: event_tx.clone(),
                    pending: Arc::new(PendingPermissions::default()),
                };

                let sid = SessionId::from("sess-test");

                let updates_and_names: Vec<(SessionUpdate, &'static str)> = vec![
                    (
                        SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
                            "step 1",
                            PlanEntryPriority::High,
                            PlanEntryStatus::Pending,
                        )])),
                        event_names::PLAN,
                    ),
                    (
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new("hi")),
                        )),
                        event_names::AGENT_MESSAGE_CHUNK,
                    ),
                    (
                        SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new("thinking")),
                        )),
                        event_names::AGENT_THOUGHT_CHUNK,
                    ),
                    (
                        SessionUpdate::UserMessageChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new("hello")),
                        )),
                        event_names::USER_MESSAGE_CHUNK,
                    ),
                    (
                        SessionUpdate::ToolCall(ToolCall::new(
                            ToolCallId::from("tc-1"),
                            "bash",
                        )),
                        event_names::TOOL_CALL,
                    ),
                    (
                        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                            ToolCallId::from("tc-1"),
                            ToolCallUpdateFields::new(),
                        )),
                        event_names::TOOL_CALL_UPDATE,
                    ),
                    (
                        SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                            SessionModeId::from("yolo"),
                        )),
                        event_names::CURRENT_MODE_UPDATE,
                    ),
                ];

                for (update, expected_name) in updates_and_names {
                    handler
                        .session_notification(SessionNotification::new(sid.clone(), update))
                        .await
                        .expect("handler ok");

                    let event = event_rx.recv().await.expect("event delivered");
                    match event {
                        AgentEvent::UserEvent { name, source, .. } => {
                            assert_eq!(name, expected_name);
                            assert_eq!(source, "core");
                        }
                        other => panic!("expected UserEvent, got {other:?}"),
                    }
                }
            })
            .await;
    }

    /// A permission request round-trips through the pending queue:
    /// the handler emits `ark.acp.permission_requested`, parks on
    /// the oneshot, and resolves when `permit(...)` fires.
    #[tokio::test(flavor = "current_thread")]
    async fn permission_request_parks_until_permit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                use agent_client_protocol::{PermissionOption, PermissionOptionKind};

                let (event_tx, mut event_rx) = broadcast::channel::<AgentEvent>(64);
                let pending = Arc::new(PendingPermissions::default());
                let handler = ClientHandler {
                    event_tx: event_tx.clone(),
                    pending: Arc::clone(&pending),
                };

                let sid = SessionId::from("s1");
                let options = vec![PermissionOption::new(
                    PermissionOptionId::from("allow-once"),
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                )];
                let req = RequestPermissionRequest::new(
                    sid.clone(),
                    agent_client_protocol::ToolCallUpdate::new(
                        agent_client_protocol::ToolCallId::from("tc-x"),
                        agent_client_protocol::ToolCallUpdateFields::new(),
                    ),
                    options,
                );

                // Spawn the request and let it park on the oneshot
                // so we can drive the permit path from the main task.
                let handler_rc = std::rc::Rc::new(handler);
                let handler_for_task = std::rc::Rc::clone(&handler_rc);
                let req_task = tokio::task::spawn_local(async move {
                    handler_for_task.request_permission(req).await
                });

                // The handler fires the event BEFORE awaiting the
                // oneshot — drive the executor once so the event
                // lands.
                tokio::task::yield_now().await;
                let event = event_rx.recv().await.expect("event delivered");
                let request_id = match &event {
                    AgentEvent::UserEvent { name, payload, source } => {
                        assert_eq!(name, event_names::PERMISSION_REQUESTED);
                        assert_eq!(source, "core");
                        payload
                            .get("request_id")
                            .and_then(|v| v.as_str())
                            .expect("request_id embedded")
                            .to_string()
                    }
                    other => panic!("expected UserEvent, got {other:?}"),
                };

                pending
                    .respond(
                        &request_id,
                        PermitOutcome::Selected {
                            option_id: "allow-once".into(),
                        }
                        .into_response(),
                    )
                    .expect("respond ok");

                let resp = req_task.await.expect("task joined").expect("permit ok");
                match resp.outcome {
                    RequestPermissionOutcome::Selected(sel) => {
                        assert_eq!(sel.option_id.0.as_ref(), "allow-once");
                    }
                    other => panic!("expected Selected, got {other:?}"),
                }
            })
            .await;
    }
}
