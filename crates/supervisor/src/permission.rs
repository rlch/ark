//! ACP permission dispatcher — 5-tier precedence (cavekit-scene R17).
//!
//! Implements the Zed-style tool-permission flow documented in R17:
//!
//! ```text
//! Tier 1: security-deny rule          (v0.4+ — stubbed to "no match")
//! Tier 2: auto-deny scene rule        (scene reaction → acp_permit)
//! Tier 3: auto-confirm scene rule     (forces picker fallback)
//! Tier 4: auto-allow scene rule       (scene reaction → acp_permit)
//! Tier 5: picker fallback             (user decides via picker plugin)
//! ```
//!
//! The dispatcher itself does NOT own the tier rules. Scene reactions
//! (T-5.3 reaction dispatcher) consume every
//! `ark.acp.permission_requested` event the dispatcher re-publishes
//! and fire `acp_permit` ops through the normal reaction pipeline.
//! The dispatcher's job is to:
//!
//! 1. Track every in-flight permission request by `request_id`.
//! 2. Arm a per-request timeout (cavekit T-ACP.5b) that fires
//!    `acp_permit request_id="…" outcome="selected" option_id="timeout"`
//!    when the window expires — that's the `UserEvent:ark.acp.permission_timeout`
//!    telemetry emission point.
//! 3. Drop late `acp_permit` responses (arriving after the timeout or
//!    after `session/cancel` wound the request down) with
//!    `tracing::debug!` — the picker plugin is expected to re-check
//!    validity before calling the op, but the dispatcher is the
//!    last-line check that guarantees no stray response leaks into
//!    the ACP client.
//!
//! # Tier 1 stub
//!
//! v0.3 ships without a security policy — a future tier layers in a
//! rule engine (e.g. from a `security-deny { }` scene block or a
//! system-wide config). Today the tier-1 check always returns "no
//! match", so the dispatcher falls through to scene rules + picker.
//! The stub is intentionally a dedicated function so the v0.4
//! upgrade only touches one call-site.
//!
//! # v0.3 surface
//!
//! The five-tier ordering is expressed here so downstream consumers
//! see one authoritative walk; the actual tier-2/3/4 evaluation is
//! the responsibility of scene reactions (ark's
//! `ReactionDispatcher` fires every reaction whose selector matches
//! the re-published event, in scene order). That keeps the
//! dispatcher small + side-effect-free; all the compiler-known
//! scene rules are already evaluated by the reaction dispatcher that
//! subscribed to the same event bus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acp_client::{AcpClient, AcpError, PermitOutcome};
use ark_types::AgentEvent;
use ark_types::EventSink;
use tokio::sync::{Mutex, broadcast};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use acp_client::event_names;

/// Number of in-flight requests the dispatcher tracks before it
/// emits a warn + drops the oldest. Matches ACP client's permission
/// slot budget; realistic agents rarely keep more than a handful of
/// open permission requests simultaneously.
pub const PENDING_REQUEST_CAPACITY: usize = 64;

/// Option id the dispatcher uses when auto-rejecting on timeout
/// (T-ACP.5b). Stable wire string — scene authors observe it on the
/// follow-up `ark.acp.permission_timeout` event.
pub const TIMEOUT_OPTION_ID: &str = "timeout";

/// Name of the user-event emitted when a permission request times
/// out. Matches the R17 telemetry surface.
pub const PERMISSION_TIMEOUT_EVENT: &str = "ark.acp.permission_timeout";

/// Classification of a tool name against the v0.3-stub security
/// policy (tier 1).
///
/// Currently always returns `AllowContinue` because no security
/// policy shipped yet. A future `v0.4+` task wires in a real rule
/// engine that inspects `tool`, `params`, and session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier1Outcome {
    /// No security-deny rule matched; fall through to later tiers.
    AllowContinue,
    /// A security-deny rule matched; immediately reject with
    /// `option_id = "security-deny"` (v0.4+).
    ///
    /// Reserved for v0.4+; never returned by the v0.3 stub but
    /// included here so the upgrade path has a pinned enum variant.
    #[allow(dead_code)]
    Deny,
}

/// Evaluate tier 1 (security-deny). v0.3 stub — always returns
/// `AllowContinue`. Future revisions read a structured rule list
/// from config + scene metadata.
pub fn evaluate_tier1(_tool: &str, _params: &serde_json::Value) -> Tier1Outcome {
    Tier1Outcome::AllowContinue
}

/// Per-request state the dispatcher carries.
#[derive(Debug)]
struct PendingRequest {
    /// Tool name from the inbound permission request (for
    /// diagnostics / timeout telemetry).
    tool: String,
    /// Wall-clock deadline after which the dispatcher auto-rejects
    /// with `option_id = "timeout"`. `None` = timeout disabled (e.g.
    /// non-interactive spawn).
    deadline: Option<Instant>,
    /// Set once a decision is routed — further responses (late
    /// picker, late scene reaction) drop silently.
    resolved: bool,
}

/// Dispatcher for ACP `session/request_permission` events.
///
/// Subscribes to the event bus, tracks requests in flight, arms
/// timers per R17 T-ACP.5b, and drops late responses. The dispatcher
/// NEVER calls `acp_permit` directly — scene reactions + the picker
/// plugin do — but it serves as the gate that keeps a stale
/// `request_id` from reaching the ACP client.
#[derive(Clone)]
pub struct PermissionDispatcher {
    inner: Arc<Mutex<PermissionDispatcherInner>>,
    acp: Arc<acp_client_handle::Slot>,
    default_timeout: Duration,
}

/// Interior state (locked).
struct PermissionDispatcherInner {
    pending: HashMap<String, PendingRequest>,
}

/// Tiny module that wraps the optional `Arc<AcpClient>` so the
/// dispatcher can be constructed before the supervisor spawns the
/// engine subprocess. The slot is swapped in at
/// supervisor-boot once the client is alive.
pub(crate) mod acp_client_handle {
    use super::*;

    /// Arc-holder for the optional ACP client. Clones cheaply.
    #[derive(Default, Debug)]
    pub struct Slot {
        inner: std::sync::Mutex<Option<Arc<AcpClient>>>,
    }

    impl Slot {
        /// Install a client, returning any prior one.
        pub fn install(&self, client: Arc<AcpClient>) -> Option<Arc<AcpClient>> {
            self.inner
                .lock()
                .expect("acp client slot mutex poisoned")
                .replace(client)
        }

        /// Current client, if installed.
        pub fn get(&self) -> Option<Arc<AcpClient>> {
            self.inner
                .lock()
                .expect("acp client slot mutex poisoned")
                .clone()
        }
    }
}

impl std::fmt::Debug for PermissionDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionDispatcher")
            .field("default_timeout", &self.default_timeout)
            .finish()
    }
}

impl PermissionDispatcher {
    /// Construct a dispatcher with the supplied per-request timeout.
    ///
    /// Pass `Duration::ZERO` to disable timeouts — the spec documents
    /// this as the non-interactive (CI / headless) mode. The ACP
    /// client slot is supplied separately at
    /// [`PermissionDispatcher::install_client`].
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PermissionDispatcherInner {
                pending: HashMap::with_capacity(PENDING_REQUEST_CAPACITY),
            })),
            acp: Arc::new(acp_client_handle::Slot::default()),
            default_timeout,
        }
    }

    /// Install (or swap) the ACP client. Returns the previous handle
    /// if any — dropped by the caller.
    pub fn install_client(&self, client: Arc<AcpClient>) -> Option<Arc<AcpClient>> {
        self.acp.install(client)
    }

    /// Handle an inbound `ark.acp.permission_requested` event.
    ///
    /// Registers the request in the tracker and arms its timeout.
    /// Tier 1 (security-deny) runs synchronously before the tracker
    /// insertion — a matching deny rule causes the dispatcher to
    /// emit a `security-deny` `acp_permit` immediately and NOT
    /// register a tracker entry (late responses would short-circuit
    /// on the unknown `request_id` already).
    ///
    /// Returns `true` when the request was accepted into the
    /// tracker; `false` when tier 1 (security-deny) resolved it
    /// early or the payload was malformed.
    pub async fn handle_request(
        &self,
        request_id: &str,
        tool: &str,
        params: &serde_json::Value,
    ) -> bool {
        if request_id.is_empty() {
            warn!(
                target: "supervisor::permission",
                "permission request carried empty request_id — dropping"
            );
            return false;
        }

        // Tier 1: security-deny stub. v0.3 always returns
        // AllowContinue — a future tier reads a rule engine.
        match evaluate_tier1(tool, params) {
            Tier1Outcome::Deny => {
                // v0.4+ path — fires immediately with a dedicated
                // option id. Scene reactions observe via the same
                // event bus and can log / audit.
                debug!(
                    target: "supervisor::permission",
                    request_id,
                    tool,
                    "tier 1 (security-deny): matched — auto-rejecting"
                );
                if let Some(client) = self.acp.get() {
                    let _ = client
                        .permit(
                            request_id,
                            PermitOutcome::Selected {
                                option_id: "security-deny".into(),
                            },
                        )
                        .await;
                }
                return false;
            }
            Tier1Outcome::AllowContinue => {}
        }

        let deadline = if self.default_timeout.is_zero() {
            None
        } else {
            Some(Instant::now() + self.default_timeout)
        };
        let mut inner = self.inner.lock().await;
        if inner.pending.len() >= PENDING_REQUEST_CAPACITY {
            warn!(
                target: "supervisor::permission",
                count = inner.pending.len(),
                cap = PENDING_REQUEST_CAPACITY,
                "pending-request map hit capacity; eviction order is undefined"
            );
        }
        inner.pending.insert(
            request_id.to_string(),
            PendingRequest {
                tool: tool.to_string(),
                deadline,
                resolved: false,
            },
        );
        debug!(
            target: "supervisor::permission",
            request_id,
            tool,
            ?deadline,
            "permission request accepted into tracker (tiers 2–5 evaluate via scene reactions)"
        );
        true
    }

    /// Probe the tracker and return `true` when `request_id` is still
    /// active (unresolved, not expired).
    ///
    /// The picker plugin / scene `acp_permit` op calls this **before**
    /// firing the op — a `false` answer means the request was timed
    /// out or cancelled by the time the user clicked through, and
    /// the picker should drop its own prompt silently.
    pub async fn is_active(&self, request_id: &str) -> bool {
        let inner = self.inner.lock().await;
        inner
            .pending
            .get(request_id)
            .map(|r| !r.resolved)
            .unwrap_or(false)
    }

    /// Mark `request_id` as resolved (a decision reached the ACP
    /// client). Late responses on the same id are dropped in
    /// [`Self::is_active`].
    pub async fn mark_resolved(&self, request_id: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(r) = inner.pending.get_mut(request_id) {
            r.resolved = true;
        }
    }

    /// Background task — scans the tracker at a fixed cadence for
    /// expired deadlines and fires `acp_permit outcome=selected
    /// option_id="timeout"` on each.
    ///
    /// Designed to run until the supplied `cancel` token fires or
    /// the bus sender is dropped. Returns when either happens.
    pub async fn run_timeout_pump(
        self,
        cancel: tokio_util::sync::CancellationToken,
        event_sink: EventSink,
    ) {
        // 1s tick granularity is plenty — permission timeouts are
        // measured in minutes, and shorter ticks just burn CPU for
        // no benefit.
        const TICK: Duration = Duration::from_millis(1000);
        let mut ticker = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    self.expire_due(&event_sink).await;
                }
            }
        }
        info!(target: "supervisor::permission", "permission timeout pump exited");
    }

    async fn expire_due(&self, event_sink: &EventSink) {
        let now = Instant::now();
        let expired: Vec<(String, String)> = {
            let mut inner = self.inner.lock().await;
            let mut out = Vec::new();
            for (id, req) in inner.pending.iter_mut() {
                if req.resolved {
                    continue;
                }
                if let Some(deadline) = req.deadline
                    && now >= deadline
                {
                    req.resolved = true;
                    out.push((id.clone(), req.tool.clone()));
                }
            }
            out
        };

        for (request_id, tool) in expired {
            warn!(
                target: "supervisor::permission",
                request_id = %request_id,
                tool = %tool,
                "permission request expired — auto-rejecting with option_id=\"timeout\""
            );
            // Emit the telemetry event so scene reactions (or the
            // audit log consumer) observe the timeout.
            let payload = serde_json::json!({
                "request_id": request_id,
                "tool": tool,
            });
            let _ = event_sink.send(AgentEvent::UserEvent {
                name: PERMISSION_TIMEOUT_EVENT.to_string(),
                payload,
                source: "core".to_string(),
            });
            // Route the auto-reject through the ACP client directly
            // — scene reactions observing the timeout event may also
            // try to fire `acp_permit`, but by then `is_active`
            // returns false and they no-op.
            if let Some(client) = self.acp.get() {
                let res = client
                    .permit(
                        &request_id,
                        PermitOutcome::Selected {
                            option_id: TIMEOUT_OPTION_ID.into(),
                        },
                    )
                    .await;
                match res {
                    Ok(()) => {}
                    Err(AcpError::UnknownPermissionRequest(_)) => {
                        debug!(
                            target: "supervisor::permission",
                            request_id = %request_id,
                            "permission request already resolved upstream — drop late timeout"
                        );
                    }
                    Err(err) => {
                        warn!(
                            target: "supervisor::permission",
                            request_id = %request_id,
                            error = %err,
                            "auto-timeout permit dispatch failed"
                        );
                    }
                }
            }
        }
    }
}

/// Spawn the event-subscription task that wires broadcast events
/// into [`PermissionDispatcher::handle_request`].
///
/// The returned `JoinHandle` should be dropped when the supervisor
/// tears down (the select loop exits on `cancel` or sender-closed).
pub fn spawn_request_watcher(
    dispatcher: PermissionDispatcher,
    mut rx: broadcast::Receiver<AgentEvent>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = rx.recv() => match recv {
                    Ok(AgentEvent::UserEvent { name, payload, .. })
                        if name == event_names::PERMISSION_REQUESTED =>
                    {
                        let (request_id, tool, params) = extract_request_fields(&payload);
                        if let Some(request_id) = request_id {
                            dispatcher
                                .handle_request(&request_id, tool.as_deref().unwrap_or(""), &params)
                                .await;
                        } else {
                            warn!(
                                target: "supervisor::permission",
                                "permission_requested event missing request_id field"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            target: "supervisor::permission",
                            skipped = n,
                            "permission watcher: bus lagged; permission requests may be stale"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    })
}

/// Extract the `(request_id, tool, params)` triple from a
/// permission-requested event payload. Returns `(None, …, Value::Null)`
/// when the payload is malformed.
fn extract_request_fields(
    payload: &serde_json::Value,
) -> (Option<String>, Option<String>, serde_json::Value) {
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let tool = payload
        .pointer("/tool_call/name")
        .or_else(|| payload.get("tool"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let params = payload
        .pointer("/tool_call/params")
        .or_else(|| payload.get("params"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    (request_id, tool, params)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier1_stub_always_returns_continue() {
        assert_eq!(
            evaluate_tier1("any-tool", &serde_json::json!({"x": 1})),
            Tier1Outcome::AllowContinue
        );
    }

    #[tokio::test]
    async fn handle_request_accepts_and_tracks() {
        let d = PermissionDispatcher::new(Duration::from_secs(60));
        let accepted = d
            .handle_request("r1", "Read", &serde_json::json!({}))
            .await;
        assert!(accepted);
        assert!(d.is_active("r1").await);
        assert!(!d.is_active("unknown").await);
    }

    #[tokio::test]
    async fn handle_request_rejects_empty_request_id() {
        let d = PermissionDispatcher::new(Duration::from_secs(60));
        let accepted = d.handle_request("", "Read", &serde_json::json!({})).await;
        assert!(!accepted);
    }

    #[tokio::test]
    async fn mark_resolved_drops_from_active_set() {
        let d = PermissionDispatcher::new(Duration::from_secs(60));
        d.handle_request("r1", "Read", &serde_json::json!({}))
            .await;
        assert!(d.is_active("r1").await);
        d.mark_resolved("r1").await;
        assert!(!d.is_active("r1").await);
    }

    #[tokio::test]
    async fn zero_timeout_disables_deadline() {
        let d = PermissionDispatcher::new(Duration::ZERO);
        d.handle_request("r1", "Read", &serde_json::json!({}))
            .await;
        // Inner deadline is None — don't panic on expire pass.
        let (sink, _rx) = ark_types::channel(4);
        d.expire_due(&sink).await;
        // Still active.
        assert!(d.is_active("r1").await);
    }

    #[tokio::test]
    async fn expire_due_emits_timeout_event_and_marks_resolved() {
        // 10ms timeout so the test stays fast.
        let d = PermissionDispatcher::new(Duration::from_millis(10));
        d.handle_request("r1", "Write", &serde_json::json!({}))
            .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        let (sink, mut rx) = ark_types::channel(4);
        d.expire_due(&sink).await;
        // Event fired.
        match rx.try_recv() {
            Ok(AgentEvent::UserEvent { name, payload, .. }) => {
                assert_eq!(name, PERMISSION_TIMEOUT_EVENT);
                assert_eq!(payload.get("request_id").unwrap().as_str(), Some("r1"));
                assert_eq!(payload.get("tool").unwrap().as_str(), Some("Write"));
            }
            other => panic!("expected UserEvent, got {other:?}"),
        }
        // Request is now resolved (subsequent responses drop).
        assert!(!d.is_active("r1").await);
    }

    #[test]
    fn extract_request_fields_handles_full_payload() {
        let payload = serde_json::json!({
            "request_id": "r42",
            "tool_call": {
                "name": "Write",
                "params": {"path": "/tmp/foo"}
            }
        });
        let (id, tool, params) = extract_request_fields(&payload);
        assert_eq!(id.as_deref(), Some("r42"));
        assert_eq!(tool.as_deref(), Some("Write"));
        assert_eq!(params.get("path").unwrap().as_str(), Some("/tmp/foo"));
    }

    #[test]
    fn extract_request_fields_flat_shape_also_works() {
        let payload = serde_json::json!({
            "request_id": "r43",
            "tool": "Read",
            "params": {"limit": 3}
        });
        let (id, tool, params) = extract_request_fields(&payload);
        assert_eq!(id.as_deref(), Some("r43"));
        assert_eq!(tool.as_deref(), Some("Read"));
        assert_eq!(params.get("limit").unwrap().as_u64(), Some(3));
    }

    #[test]
    fn extract_request_fields_missing_id_returns_none() {
        let payload = serde_json::json!({"tool": "Read"});
        let (id, _, _) = extract_request_fields(&payload);
        assert!(id.is_none());
    }
}
