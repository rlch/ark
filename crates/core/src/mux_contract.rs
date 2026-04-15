//! Multiplexer contract suite — trait-level conformance tests that every
//! [`crate::Multiplexer`] implementation must pass.
//!
//! Implements cavekit-architecture.md R1/R4 (T-116). Mirrors the
//! [`crate::engine_contract`] / [`crate::orchestrator_contract`] patterns:
//! each multiplexer impl wires a factory that yields a fresh mux paired
//! with a command recorder (typically wrapping the impl's stub executor),
//! and the suite drives scripted scenarios through trait methods while
//! asserting the recorded argv shape.
//!
//! The `Multiplexer` trait does not itself expose an executor slot (the
//! trait is deliberately argv-agnostic so a future `TmuxMux` can ship
//! without trait churn per cavekit-architecture.md R6). To avoid adding
//! test-only surface to the trait, the contract suite consumes a
//! [`MuxHarness`] adapter built by the impl crate. For zellij that means
//! wrapping an `Arc<StubExecutor>`; for a future tmux impl it would mean
//! wrapping whatever recording executor the tmux crate ships.
//!
//! ## Scenarios
//!
//! | Scenario                                       | Trait method exercised | Spec |
//! |-----------------------------------------------|------------------------|------|
//! | `factory_produces_fresh_instance`             | (factory closure)      | R4   |
//! | `kind_is_stable_non_empty_slug`               | `kind`                 | R4   |
//! | `ensure_session_switch_has_no_create_flag`    | `ensure_session`       | mux-zellij R1 / Q5 |
//! | `create_tab_new_session_argv_includes_s_flag` | `create_tab`           | mux-zellij R1/R2  |
//! | `create_tab_additional_uses_new_tab_action`   | `create_tab`           | mux-zellij R2     |
//! | `create_tab_preserves_kdl_extension`          | `create_tab`           | mux-zellij R1/R5  |
//! | `pipe_argv_shape`                             | `pipe`                 | mux-zellij R4     |
//!
//! The `ensure_session` / `create_tab` switch-session scenarios cover the
//! `--create`-absence constraint from cavekit-mux-zellij.md Q5 (and R1
//! acceptance line 20): `switch-session` create-if-missing is the DEFAULT,
//! so re-adding `--create` (which only exists on `attach`) would surface a
//! regression only a contract suite can catch.
//!
//! The `.kdl` scenario asserts the impl does not strip the extension from
//! the argv it constructs; the stronger enforcement (reject non-`.kdl`
//! paths up front) lives in each impl's own resolver / writer layer
//! (cavekit-mux-zellij R5).

use std::path::Path;
use std::sync::Arc;

use crate::multiplexer::Multiplexer;

/// Recorded `(program, args)` pair from a single call through a mux's
/// command executor. Comparable so the contract suite can assert exact
/// argv shapes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedCall {
    pub program: String,
    pub args: Vec<String>,
}

impl RecordedCall {
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
        }
    }

    /// True if `arg` appears anywhere in the argv.
    pub fn argv_contains(&self, arg: &str) -> bool {
        self.args.iter().any(|a| a == arg)
    }
}

/// A harness bundles a concrete `Multiplexer` with a side-channel into
/// whatever command recorder the impl uses. The contract suite drives
/// trait methods through `mux()` and asserts on `recorded_calls()`.
///
/// Impl crates are expected to queue "success" responses via
/// `queue_success()` before each scripted scenario so the underlying
/// executor doesn't synthesize I/O errors.
pub trait MuxHarness: Send + Sync {
    /// Borrow the mux under test.
    fn mux(&self) -> &dyn Multiplexer;

    /// Queue a single successful (exit-status zero, empty stdout/stderr)
    /// response on the underlying stub executor. The contract suite calls
    /// this before each scenario that would otherwise exercise real I/O.
    fn queue_success(&self);

    /// Queue a successful response with the given stdout bytes — used by
    /// scenarios that need the mux to parse output (e.g. `list-sessions`).
    fn queue_success_stdout(&self, stdout: &[u8]);

    /// Snapshot of every `(program, args)` pair the mux has issued since
    /// construction.
    fn recorded_calls(&self) -> Vec<RecordedCall>;
}

/// Run the portable Multiplexer contract suite against `factory`.
///
/// Each invocation of `factory` must mint a fresh, independent
/// `(Multiplexer, recorder)` pair. Reusing a single harness across
/// scenarios would let recorded calls bleed between assertions and is a
/// contract violation.
///
/// # Panics
/// Panics on the first violated assertion. Tests convert panics into
/// failures, so this is the intended failure mode.
pub fn mux_contract_suite<F>(factory: F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    factory_produces_fresh_instance(&factory);
    kind_is_stable_non_empty_slug(&factory);
    ensure_session_switch_has_no_create_flag(&factory);
    create_tab_new_session_argv_includes_s_flag(&factory);
    create_tab_additional_uses_new_tab_action(&factory);
    create_tab_preserves_kdl_extension(&factory);
    pipe_argv_shape(&factory);
}

fn factory_produces_fresh_instance<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let a = factory();
    let b = factory();
    assert_eq!(
        a.mux().kind(),
        b.mux().kind(),
        "factory must produce muxes of the same kind (got `{}` and `{}`)",
        a.mux().kind(),
        b.mux().kind()
    );
    // Two fresh harnesses must have independent recorders: neither should
    // see the other's calls. We verify by issuing a single call through
    // `a` and asserting `b` remains empty.
    a.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        // `kind` is a pure accessor — issue an actual trait call that
        // touches the recorder via `pipe` (non-fatal on every impl).
        a.queue_success();
        let _ = a.mux().pipe("ark-contract-isolation", "{}").await;
    });
    let a_calls = a.recorded_calls();
    let b_calls = b.recorded_calls();
    assert!(
        !a_calls.is_empty(),
        "harness a must record its own pipe call (got zero)"
    );
    assert!(
        b_calls.is_empty(),
        "harness b must NOT see harness a's calls — factory returned a shared recorder? \
         b recorded: {b_calls:?}"
    );
}

fn kind_is_stable_non_empty_slug<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    let k = h.mux().kind();
    assert!(
        !k.is_empty(),
        "Multiplexer::kind must return a non-empty &'static str"
    );
    assert!(
        k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Multiplexer::kind must be a slug (lowercase ascii + digits + dash), got {k:?}"
    );
    assert_eq!(
        h.mux().kind(),
        k,
        "Multiplexer::kind must be stable across calls on the same instance"
    );
}

/// Spec: cavekit-mux-zellij.md R1 + Q5. When a mux switches sessions,
/// the argv MUST NOT carry `--create`. Zellij's `switch-session`
/// create-if-missing is the default; `--create` only exists on
/// `attach`. Any impl that maps `ensure_session` onto
/// `switch-session`-style semantics should pass this check; impls that
/// use a different idiom (e.g. a hypothetical tmux impl) pass
/// vacuously because their recorded argv simply won't carry
/// `--create`.
fn ensure_session_switch_has_no_create_flag<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    h.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        h.mux()
            .ensure_session("ark-contract-sess")
            .await
            .expect("ensure_session must succeed on a stubbed successful run");
    });
    let calls = h.recorded_calls();
    assert!(
        !calls.is_empty(),
        "ensure_session must issue at least one recorded command"
    );
    for call in &calls {
        assert!(
            !call.argv_contains("--create"),
            "ensure_session argv must NOT carry `--create` (zellij issue: \
             switch-session takes no --create; that flag exists on attach). \
             Offending call: {} {:?}",
            call.program,
            call.args
        );
    }
}

/// Spec: cavekit-mux-zellij.md R1/R2. First `create_tab` against a
/// previously-unseen session must carry the session name through the
/// argv — either via `-s <name>` (outside-zellij spawn) or via
/// `switch-session <name>` (inside-zellij spawn). We assert the session
/// name appears verbatim in the argv so a future refactor that drops it
/// is caught immediately.
fn create_tab_new_session_argv_includes_s_flag<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    h.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let session = "ark-contract-fresh";
    rt.block_on(async {
        h.mux()
            .create_tab(session, "builder", Path::new("/tmp/ark-contract.kdl"))
            .await
            .expect("create_tab must succeed on a stubbed successful run");
    });
    let calls = h.recorded_calls();
    assert!(
        !calls.is_empty(),
        "create_tab must issue at least one recorded command"
    );
    let carries_session = calls.iter().any(|c| c.argv_contains(session));
    assert!(
        carries_session,
        "first create_tab must carry session name `{session}` in its argv; \
         recorded: {calls:?}"
    );
}

/// Spec: cavekit-mux-zellij.md R2. The second `create_tab` in a
/// session must use the additional-tab idiom (`action new-tab`) rather
/// than re-spawning the session. We don't hard-code the exact argv
/// shape (different impls may order flags differently), but assert
/// `new-tab` appears in the second call's argv.
fn create_tab_additional_uses_new_tab_action<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    // Two successes: one for the first tab (session spawn) and one for
    // the second (new-tab action).
    h.queue_success();
    h.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let session = "ark-contract-two-tabs";
    rt.block_on(async {
        h.mux()
            .create_tab(session, "builder", Path::new("/tmp/ark-contract.kdl"))
            .await
            .expect("first create_tab must succeed");
        h.mux()
            .create_tab(session, "review", Path::new("/tmp/ark-contract.kdl"))
            .await
            .expect("second create_tab must succeed");
    });
    let calls = h.recorded_calls();
    assert!(
        calls.len() >= 2,
        "two create_tab calls must record at least two commands, got {}: {calls:?}",
        calls.len()
    );
    let second = calls.last().unwrap();
    assert!(
        second.argv_contains("new-tab"),
        "second create_tab must use the `new-tab` action; recorded: {} {:?}",
        second.program,
        second.args
    );
    assert!(
        second.argv_contains("review"),
        "second create_tab argv must carry the tab name; recorded: {} {:?}",
        second.program,
        second.args
    );
}

/// Spec: cavekit-mux-zellij.md R1/R5. Passing a `.kdl` layout path
/// must produce argv that preserves the `.kdl` extension verbatim —
/// zellij issue #4994 silently ignores non-`.kdl` layouts, so any
/// impl that mangles the extension would break silently in
/// production.
fn create_tab_preserves_kdl_extension<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    h.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let layout = "/tmp/ark-contract-layout-xyz.kdl";
    rt.block_on(async {
        h.mux()
            .create_tab("ark-contract-kdl", "builder", Path::new(layout))
            .await
            .expect("create_tab with .kdl path must succeed");
    });
    let calls = h.recorded_calls();
    let carries_kdl = calls.iter().any(|c| c.argv_contains(layout));
    assert!(
        carries_kdl,
        "create_tab argv must carry the `.kdl` layout path verbatim \
         ({layout}); recorded: {calls:?}"
    );
}

/// Spec: cavekit-mux-zellij.md R4. `pipe(target, payload)` must
/// carry both the target name and the payload through the argv so the
/// plugin receives the exact bytes the supervisor intended.
fn pipe_argv_shape<F>(factory: &F)
where
    F: Fn() -> Arc<dyn MuxHarness>,
{
    let h = factory();
    h.queue_success();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let target = "ark-contract-target";
    let payload = "{\"k\":\"v\"}";
    rt.block_on(async {
        h.mux()
            .pipe(target, payload)
            .await
            .expect("pipe must succeed on a stubbed successful run");
    });
    let calls = h.recorded_calls();
    assert!(
        !calls.is_empty(),
        "pipe must issue at least one recorded command"
    );
    let carries_target = calls.iter().any(|c| c.argv_contains(target));
    let carries_payload = calls.iter().any(|c| c.argv_contains(payload));
    assert!(
        carries_target,
        "pipe argv must carry target name `{target}`; recorded: {calls:?}"
    );
    assert!(
        carries_payload,
        "pipe argv must carry payload verbatim; recorded: {calls:?}"
    );
}

#[cfg(test)]
mod tests {
    //! The contract suite is itself exercised against a minimal
    //! recording MockMux so we prove the suite's assertions run and
    //! fail-loud. The real-impl exercise lives in
    //! `crates/mux/zellij/tests/contract.rs`.

    use super::*;
    use crate::multiplexer::Multiplexer;
    use ark_types::TabHandle;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// In-memory mux modelled on the zellij argv shape so the portable
    /// contract can exercise every scenario without depending on the
    /// zellij crate.
    #[derive(Default)]
    struct MockMux {
        calls: Mutex<Vec<RecordedCall>>,
        responses: Mutex<VecDeque<()>>,
        sessions_spawned: Mutex<std::collections::BTreeSet<String>>,
    }

    impl MockMux {
        fn record(&self, program: &str, args: &[&str]) {
            self.calls.lock().unwrap().push(RecordedCall::new(
                program,
                args.iter().map(|s| s.to_string()),
            ));
        }

        fn pop_response(&self) -> anyhow::Result<()> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no queued response"))
        }
    }

    #[async_trait]
    impl Multiplexer for MockMux {
        fn kind(&self) -> &'static str {
            "mock"
        }

        async fn ensure_session(&self, name: &str) -> anyhow::Result<()> {
            // Mirror the zellij inside-zellij path: action switch-session <name>,
            // deliberately without a `--create` flag.
            self.record("mock-mux", &["action", "switch-session", name]);
            self.pop_response()
        }

        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            layout_path: &Path,
        ) -> anyhow::Result<TabHandle> {
            let layout = layout_path.to_str().expect("utf-8 layout path");
            let mut spawned = self.sessions_spawned.lock().unwrap();
            if !spawned.contains(session) {
                // Mirror outside-zellij first-tab shape.
                self.record(
                    "mock-mux",
                    &["-s", session, "--layout", layout, "--name", name],
                );
                spawned.insert(session.to_string());
            } else {
                // Additional tab.
                self.record(
                    "mock-mux",
                    &[
                        "--session",
                        session,
                        "action",
                        "new-tab",
                        "--layout",
                        layout,
                        "--name",
                        name,
                    ],
                );
            }
            drop(spawned);
            self.pop_response()?;
            Ok(TabHandle::new(session, 0, name))
        }

        async fn close_tab(&self, handle: &TabHandle) -> anyhow::Result<()> {
            let idx = handle.tab_index.to_string();
            self.record(
                "mock-mux",
                &[
                    "--session",
                    &handle.session,
                    "action",
                    "close-tab-at-index",
                    &idx,
                ],
            );
            self.pop_response()
        }

        async fn rename_tab(&self, handle: &TabHandle, name: &str) -> anyhow::Result<()> {
            let idx = handle.tab_index.to_string();
            self.record(
                "mock-mux",
                &[
                    "--session",
                    &handle.session,
                    "action",
                    "rename-tab",
                    "--tab-index",
                    &idx,
                    "--name",
                    name,
                ],
            );
            self.pop_response()
        }

        async fn pipe(&self, target: &str, payload: &str) -> anyhow::Result<()> {
            self.record("mock-mux", &["pipe", "--name", target, "--", payload]);
            self.pop_response()
        }
    }

    struct MockHarness {
        mux: MockMux,
    }

    impl MockHarness {
        fn new() -> Self {
            Self {
                mux: MockMux::default(),
            }
        }
    }

    impl MuxHarness for MockHarness {
        fn mux(&self) -> &dyn Multiplexer {
            &self.mux
        }

        fn queue_success(&self) {
            self.mux.responses.lock().unwrap().push_back(());
        }

        fn queue_success_stdout(&self, _stdout: &[u8]) {
            self.mux.responses.lock().unwrap().push_back(());
        }

        fn recorded_calls(&self) -> Vec<RecordedCall> {
            self.mux.calls.lock().unwrap().clone()
        }
    }

    #[test]
    fn mock_mux_passes_contract_suite() {
        mux_contract_suite(|| Arc::new(MockHarness::new()));
    }

    #[test]
    fn recorded_call_argv_contains_matches() {
        let rc = RecordedCall::new(
            "zellij",
            ["action".to_string(), "switch-session".to_string()],
        );
        assert!(rc.argv_contains("action"));
        assert!(rc.argv_contains("switch-session"));
        assert!(!rc.argv_contains("--create"));
    }

    #[test]
    fn contract_rejects_mux_that_emits_create_flag() {
        /// Wrapper mux that deliberately injects `--create` on ensure_session.
        struct BadMux {
            inner: MockMux,
        }

        #[async_trait]
        impl Multiplexer for BadMux {
            fn kind(&self) -> &'static str {
                "bad"
            }
            async fn ensure_session(&self, name: &str) -> anyhow::Result<()> {
                self.inner
                    .record("bad-mux", &["action", "switch-session", name, "--create"]);
                self.inner.pop_response()
            }
            async fn create_tab(&self, _s: &str, _n: &str, _p: &Path) -> anyhow::Result<TabHandle> {
                unreachable!()
            }
            async fn close_tab(&self, _h: &TabHandle) -> anyhow::Result<()> {
                Ok(())
            }
            async fn rename_tab(&self, _h: &TabHandle, _n: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn pipe(&self, _t: &str, _p: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        struct BadHarness {
            mux: BadMux,
        }

        impl MuxHarness for BadHarness {
            fn mux(&self) -> &dyn Multiplexer {
                &self.mux
            }
            fn queue_success(&self) {
                self.mux.inner.responses.lock().unwrap().push_back(());
            }
            fn queue_success_stdout(&self, _s: &[u8]) {
                self.mux.inner.responses.lock().unwrap().push_back(());
            }
            fn recorded_calls(&self) -> Vec<RecordedCall> {
                self.mux.inner.calls.lock().unwrap().clone()
            }
        }

        let factory = || -> Arc<dyn MuxHarness> {
            Arc::new(BadHarness {
                mux: BadMux {
                    inner: MockMux::default(),
                },
            })
        };
        let result =
            std::panic::catch_unwind(|| ensure_session_switch_has_no_create_flag(&factory));
        assert!(
            result.is_err(),
            "expected mux that emits `--create` on switch-session to be rejected"
        );
    }

    #[test]
    fn contract_rejects_mux_with_empty_kind() {
        struct EmptyKindMux;

        #[async_trait]
        impl Multiplexer for EmptyKindMux {
            fn kind(&self) -> &'static str {
                ""
            }
            async fn ensure_session(&self, _n: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn create_tab(&self, _s: &str, _n: &str, _p: &Path) -> anyhow::Result<TabHandle> {
                unreachable!()
            }
            async fn close_tab(&self, _h: &TabHandle) -> anyhow::Result<()> {
                Ok(())
            }
            async fn rename_tab(&self, _h: &TabHandle, _n: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn pipe(&self, _t: &str, _p: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        struct EmptyKindHarness;

        impl MuxHarness for EmptyKindHarness {
            fn mux(&self) -> &dyn Multiplexer {
                &EmptyKindMux
            }
            fn queue_success(&self) {}
            fn queue_success_stdout(&self, _s: &[u8]) {}
            fn recorded_calls(&self) -> Vec<RecordedCall> {
                Vec::new()
            }
        }

        let factory = || -> Arc<dyn MuxHarness> { Arc::new(EmptyKindHarness) };
        let result = std::panic::catch_unwind(|| kind_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected empty-kind mux to be rejected by kind slug assertion"
        );
    }
}
