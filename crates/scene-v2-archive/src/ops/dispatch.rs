//! Op dispatch sequencing (T-4.5).
//!
//! A reaction's op body is an ordered list of ops. At fire time the
//! dispatcher walks that list in textual order, looks up each op in the
//! [`IntentRegistry`] by name, and awaits its `dispatch_dyn`.
//!
//! Three behaviors this module encodes:
//!
//! 1. **Order-preserving dispatch.** Ops run strictly in source order.
//!    No concurrency within a reaction — ops frequently depend on each
//!    other's side effects (`open_tab` then `mount_plugin into="..."`),
//!    and R4's reaction-cascade semantics assume serial ops per
//!    reaction.
//!
//! 2. **Fail-fast.** The FIRST op failure stops the sequence. The
//!    remaining ops in the same reaction are skipped. The op failure
//!    is logged via `tracing::error!(target = "scene::ops", ...)` with
//!    the reaction origin + op kind + the underlying error, and a
//!    [`SceneError::OpFailed`] is returned. The CALLER (the reactions
//!    dispatcher, not this function) swallows that error and keeps the
//!    event loop alive — consistent with R4's "reactions are
//!    best-effort" rule.
//!
//! 3. **Idempotency matrix (documentation-only at this tier).** The
//!    per-op [`Idempotency`][super::Idempotency] class is surfaced via
//!    [`CompiledOp::idempotency`] so operational dashboards can
//!    classify ops without inspecting their implementation. The
//!    dispatcher itself does not gate on the class today — each op is
//!    responsible for honoring its documented semantics (e.g.
//!    `close_tab` silently returns `Ok(None)` when its target is
//!    absent). An explicit `if_exists="focus|create|error"` override
//!    is deferred to v0.2 per R7.

use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{IntentContext, IntentError, IntentRegistry, IntentValue};

use super::Idempotency;

/// Op name + args, resolved to a shape the dispatcher can execute.
///
/// v1 "compiled op" is thin — we hold the KDL node verbatim and the
/// resolved namespaced op name. The actual args parse happens in
/// `IntentRegistry::dispatch_dyn` via facet-kdl round-trip. A richer
/// compile step (pre-parsing args into the concrete `Args` struct)
/// lands when the op-typed-enum work (T-3.2 follow-up) unblocks it.
#[derive(Debug, Clone)]
pub struct CompiledOp {
    /// Fully qualified op name — `"ark.core.<verb>"` for built-ins,
    /// `"<ext>.<verb>"` for extension ops.
    pub name: String,

    /// Idempotency class, surfaced from the op's
    /// `const IDEMPOTENCY` declaration. Purely descriptive today;
    /// consumed by operational tooling (e.g. `ark scene graph`).
    pub idempotency: Idempotency,

    /// Raw KDL node carrying the op's arguments. Parsed into the op's
    /// typed `Args` by [`IntentRegistry::dispatch_dyn`].
    pub node: KdlNode,
}

impl CompiledOp {
    /// Construct a [`CompiledOp`] directly. Exposed so tests + the
    /// compile pipeline can build ops ad-hoc; the compile pipeline
    /// (T-4.6 / T-4.7 in the plan) will produce these from the AST.
    pub fn new(name: impl Into<String>, idempotency: Idempotency, node: KdlNode) -> Self {
        Self {
            name: name.into(),
            idempotency,
            node,
        }
    }
}

/// Execute a sequence of compiled ops against the registry, in order,
/// with fail-fast semantics.
///
/// Returns `Ok(())` after every op dispatches successfully. On the
/// first op failure, logs the error under the `scene::ops` target and
/// returns `Err(SceneError::OpFailed)`. Remaining ops are NOT
/// dispatched.
///
/// The caller (the reactions dispatcher in
/// `crates/scene/src/*` — landing in a later tier) absorbs the error
/// and keeps the event loop alive. This function is deliberately
/// unaware of reaction-graph state; it's a thin "run this list" helper
/// that keeps the fail-fast + logging policy in one place so every
/// call site agrees.
pub async fn dispatch_sequence(
    ops: &[CompiledOp],
    registry: &IntentRegistry,
    ctx: &IntentContext,
) -> Result<(), SceneError> {
    for op in ops {
        match registry.dispatch_dyn(&op.name, &op.node, ctx).await {
            Ok(_value) => {
                tracing::debug!(
                    target = "scene::ops",
                    op = %op.name,
                    idempotency = ?op.idempotency,
                    "op dispatched"
                );
            }
            Err(err) => {
                let message = err.to_string();
                tracing::error!(
                    target = "scene::ops",
                    op = %op.name,
                    idempotency = ?op.idempotency,
                    scene_id = %ctx.scene_id,
                    origin = ?ctx.origin,
                    error = %err,
                    "op/failed — reaction aborted, remaining ops skipped"
                );
                return Err(SceneError::OpFailed {
                    op: op.name.clone(),
                    message,
                });
            }
        }
    }
    Ok(())
}

/// Return value bundle for tests that want to inspect what
/// `dispatch_sequence` would have produced, plus the per-op outcome.
/// Not used by the production dispatcher (which discards intermediate
/// values; reaction cascades consume them via the registry directly).
#[derive(Debug)]
pub struct SequenceTrace {
    /// One entry per op in the input — in order. The last entry is the
    /// failing op when the trace ends on an error.
    pub outcomes: Vec<OpOutcome>,
}

/// Per-op result in a [`SequenceTrace`].
#[derive(Debug)]
pub enum OpOutcome {
    /// The op ran successfully. Value is whatever its `dispatch`
    /// returned (often `None` for stub ops at this tier).
    Ok {
        /// Op name as configured on the [`CompiledOp`].
        name: String,
        /// Op's return value, if any.
        value: Option<IntentValue>,
    },
    /// The op failed. The trace stops after this entry.
    Err {
        /// Op name.
        name: String,
        /// Underlying intent error.
        error: IntentError,
    },
}

/// Variant of [`dispatch_sequence`] that returns the per-op trace for
/// testing. Production code uses [`dispatch_sequence`].
pub async fn dispatch_sequence_trace(
    ops: &[CompiledOp],
    registry: &IntentRegistry,
    ctx: &IntentContext,
) -> SequenceTrace {
    let mut trace = SequenceTrace {
        outcomes: Vec::with_capacity(ops.len()),
    };
    for op in ops {
        match registry.dispatch_dyn(&op.name, &op.node, ctx).await {
            Ok(value) => trace.outcomes.push(OpOutcome::Ok {
                name: op.name.clone(),
                value,
            }),
            Err(error) => {
                trace.outcomes.push(OpOutcome::Err {
                    name: op.name.clone(),
                    error,
                });
                break;
            }
        }
    }
    trace
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SceneId;
    use crate::intent::{Intent, IntentContext};
    use crate::ops::{
        control::{ExecOp, ReloadSceneOp},
        messaging::{EmitOp, PipeOp, SetStatusOp},
        panes::{ClosePaneOp, SplitPaneOp},
        plugins::{MountPluginOp, UnmountPluginOp},
        register_core_ops,
        tabs::{CloseTabOp, FocusTabOp, OpenTabOp, RenameTabOp},
        Idempotency,
    };
    use ::kdl::KdlDocument;
    use std::path::PathBuf;

    fn ctx() -> IntentContext {
        IntentContext::placeholder(SceneId::from_bytes(
            PathBuf::from("/tmp/scene.kdl"),
            b"scene \"x\" { }",
        ))
    }

    fn node(src: &str) -> KdlNode {
        let doc: KdlDocument = src.parse().expect("parse");
        doc.nodes().first().cloned().expect("node")
    }

    fn compiled(name: &str, idem: Idempotency, src: &str) -> CompiledOp {
        CompiledOp::new(name, idem, node(src))
    }

    // -- happy path -----------------------------------------------------

    #[tokio::test]
    async fn sequence_runs_every_op_in_order() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;

        // open_tab + set_status: both stubs, should both succeed.
        let ops = vec![
            compiled(
                OpenTabOp::NAME,
                OpenTabOp::IDEMPOTENCY,
                r#"open_tab name="work""#,
            ),
            compiled(
                SetStatusOp::NAME,
                SetStatusOp::IDEMPOTENCY,
                r#"set_status text="ok""#,
            ),
        ];
        dispatch_sequence(&ops, &reg, &ctx())
            .await
            .expect("both succeed");
    }

    // -- fail-fast ------------------------------------------------------

    #[tokio::test]
    async fn sequence_fails_fast_on_first_error_and_skips_rest() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;

        // Op 1: bad exec (sleep 5 with 50ms timeout) → fails.
        // Op 2: emit (would succeed) — must NOT run.
        let ctx = ctx();
        let ops = vec![
            compiled(
                ExecOp::NAME,
                ExecOp::IDEMPOTENCY,
                r#"exec script="sleep 5" timeout_ms=50"#,
            ),
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.should_not_fire""#,
            ),
        ];
        let err = dispatch_sequence(&ops, &reg, &ctx)
            .await
            .expect_err("exec must fail");
        match &err {
            SceneError::OpFailed { op, message } => {
                assert_eq!(op, "ark.core.exec");
                assert!(
                    message.contains("timed out") || message.contains("exec"),
                    "message: {message:?}"
                );
            }
            other => panic!("expected OpFailed, got {other:?}"),
        }
        // Verify emit did NOT run — bus capture queue should be empty.
        assert!(
            ctx.bus.drain_user_events().is_empty(),
            "emit op should have been skipped on fail-fast"
        );
    }

    // -- trace variant --------------------------------------------------

    #[tokio::test]
    async fn trace_captures_every_outcome() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;

        let ops = vec![
            compiled(
                ExecOp::NAME,
                ExecOp::IDEMPOTENCY,
                r#"exec script="printf hi""#,
            ),
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.ok""#,
            ),
        ];
        let trace = dispatch_sequence_trace(&ops, &reg, &ctx()).await;
        assert_eq!(trace.outcomes.len(), 2);
        assert!(matches!(trace.outcomes[0], OpOutcome::Ok { .. }));
        assert!(matches!(trace.outcomes[1], OpOutcome::Ok { .. }));
    }

    #[tokio::test]
    async fn trace_stops_at_first_error() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;

        let ops = vec![
            compiled(
                ExecOp::NAME,
                ExecOp::IDEMPOTENCY,
                r#"exec script="false""#, // exits 1 — OK to exec, non-zero status, but exec op reports success; to force failure, pick a bad shell arg
            ),
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.after""#,
            ),
        ];
        // `false` exits 1 but the exec op's definition of success is "ran to
        // completion" — it returns Ok with exit_code=1. So this test uses
        // a different failure path: an unknown op in position 1.
        let ops = vec![
            CompiledOp::new(
                "ark.core.does_not_exist",
                Idempotency::AlwaysSideEffect,
                node(r#"does_not_exist"#),
            ),
            ops[1].clone(),
        ];
        let trace = dispatch_sequence_trace(&ops, &IntentRegistry::new(), &ctx()).await;
        assert_eq!(trace.outcomes.len(), 1);
        assert!(matches!(trace.outcomes[0], OpOutcome::Err { .. }));
    }

    // -- idempotency matrix coverage ------------------------------------

    /// Sanity test: every core op's declared idempotency class matches
    /// the table documented in `ops/mod.rs`. Regression guard — if
    /// someone changes a constant, this test catches the drift.
    #[test]
    fn idempotency_matrix_matches_documentation() {
        assert_eq!(OpenTabOp::IDEMPOTENCY, Idempotency::IfAbsentFocusElseCreate);
        assert_eq!(CloseTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(RenameTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(FocusTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(SplitPaneOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(ClosePaneOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(MountPluginOp::IDEMPOTENCY, Idempotency::LaunchOrFocus);
        assert_eq!(UnmountPluginOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(PipeOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(EmitOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(SetStatusOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(ExecOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(ReloadSceneOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
    }

    // -- idempotent-noop-on-absent behavior for a stubbed op ------------

    /// `close_tab name="nonexistent"` is an `idempotent-noop-on-absent`
    /// op. In our stub implementation it returns `Ok(None)` regardless
    /// of whether the tab exists, so firing it twice in a row with the
    /// same selector is a no-op pair. Captures the contract on the
    /// stub; when the real mux handle lands (T-5.x), the behavior MUST
    /// remain idempotent.
    #[tokio::test]
    async fn noop_on_absent_op_is_idempotent_twice() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;
        let ops = vec![
            compiled(
                CloseTabOp::NAME,
                CloseTabOp::IDEMPOTENCY,
                r#"close_tab name="nonexistent""#,
            ),
            compiled(
                CloseTabOp::NAME,
                CloseTabOp::IDEMPOTENCY,
                r#"close_tab name="nonexistent""#,
            ),
        ];
        dispatch_sequence(&ops, &reg, &ctx())
            .await
            .expect("idempotent no-op firing twice must succeed");
    }

    // -- always-side-effect behavior ------------------------------------

    /// `emit` is `always-side-effect`: firing it twice enqueues two
    /// events. Guards against a regression where a future
    /// optimisation might dedupe consecutive emits.
    #[tokio::test]
    async fn always_side_effect_emit_fires_every_time() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;
        let ctx = ctx();
        let ops = vec![
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.tick""#,
            ),
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.tick""#,
            ),
            compiled(
                EmitOp::NAME,
                EmitOp::IDEMPOTENCY,
                r#"emit "user.tick""#,
            ),
        ];
        dispatch_sequence(&ops, &reg, &ctx)
            .await
            .expect("each emit succeeds");
        let drained = ctx.bus.drain_user_events();
        assert_eq!(drained.len(), 3, "every emit enqueued a distinct event");
    }
}
