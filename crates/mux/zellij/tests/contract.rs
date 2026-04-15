//! Multiplexer contract suite — applied to `ZellijMux`.
//!
//! Implements cavekit-architecture.md R1/R4 and cavekit-mux-zellij.md
//! R1/R2/R4/R5 (T-116). The portable scenarios live in
//! [`ark_core::mux_contract`]; this file wires the suite to the
//! `ZellijMux` + `StubExecutor` pair and adds zellij-specific scenarios
//! (KDL layout rendering, `{{cwd}}` interpolation) on top.
//!
//! The portable suite must stay green after any refactor of the zellij
//! argv shape that preserves the contract — e.g. reordering flags,
//! renaming internal helpers, switching between inside-zellij and
//! outside-zellij spawn idioms — while catching regressions that would
//! re-introduce `--create` on `switch-session` or silently strip the
//! `.kdl` extension from a layout path.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use ark_core::{Multiplexer, MuxHarness, RecordedCall, mux_contract_suite};
use ark_mux_zellij::{
    CommandExecutor, CommandOutput, LayoutVars, StubExecutor, ZellijMux, render_layout,
};

/// Borrow a successful `ExitStatus` by running the POSIX `true` binary
/// synchronously. Synchronous so harness construction is safe to call
/// from inside a `#[tokio::test]` runtime (which forbids nested
/// `block_on`).
fn ok_status_sync() -> std::process::ExitStatus {
    std::process::Command::new("true")
        .status()
        .expect("`true` must exist on PATH")
}

/// `Arc<StubExecutor>` adapter so both the mux and the harness can share
/// the same recorder. Cloning the `Arc` hands a second reference to the
/// same `Mutex`-guarded state.
struct ArcExec(Arc<StubExecutor>);

#[async_trait]
impl CommandExecutor for ArcExec {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.0.run(program, args).await
    }
}

/// Harness: a fresh `ZellijMux` + the `Arc<StubExecutor>` it was built
/// with. The suite drives the mux through `mux()` and inspects recorded
/// argv via `recorded_calls()`.
struct ZellijHarness {
    mux: ZellijMux,
    stub: Arc<StubExecutor>,
    ok: std::process::ExitStatus,
}

impl ZellijHarness {
    fn build(in_zellij: bool) -> Arc<dyn MuxHarness> {
        // Seed a `true`-derived ExitStatus once; the harness reuses it
        // for every queued response. Synchronous so the constructor is
        // callable from inside a `#[tokio::test]` runtime.
        let ok = ok_status_sync();
        let stub = Arc::new(StubExecutor::new());
        let mux =
            ZellijMux::with_executor(Box::new(ArcExec(stub.clone()))).with_in_zellij(in_zellij);
        Arc::new(Self { mux, stub, ok })
    }
}

impl MuxHarness for ZellijHarness {
    fn mux(&self) -> &dyn Multiplexer {
        &self.mux
    }

    fn queue_success(&self) {
        self.stub.queue_response(CommandOutput {
            status: self.ok,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }

    fn queue_success_stdout(&self, stdout: &[u8]) {
        self.stub.queue_response(CommandOutput {
            status: self.ok,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        });
    }

    fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.stub
            .recorded_calls()
            .into_iter()
            .map(|(program, args)| RecordedCall { program, args })
            .collect()
    }
}

/// Full portable contract suite applied to `ZellijMux` in the
/// outside-zellij mode (the default production path on a fresh `ark
/// spawn`).
#[test]
fn zellij_passes_mux_contract_outside_zellij() {
    mux_contract_suite(|| ZellijHarness::build(false));
}

/// Full portable contract suite applied to `ZellijMux` in the
/// inside-zellij mode (supervisor invoked from within an existing zellij
/// client). This path goes through `action switch-session` for the first
/// tab and is the primary guard against the `--create` regression
/// described in cavekit-mux-zellij.md R1 / Q5.
#[test]
fn zellij_passes_mux_contract_inside_zellij() {
    mux_contract_suite(|| ZellijHarness::build(true));
}

/// Zellij-specific scenario: `switch-session` must never carry
/// `--create` even across the inside-zellij first-tab spawn (which also
/// uses `switch-session` + `--layout`). Covers cavekit-mux-zellij.md R1
/// line 20 explicitly on top of the portable suite.
#[tokio::test]
async fn zellij_switch_session_with_layout_has_no_create_flag() {
    let h = ZellijHarness::build(true);
    h.queue_success();
    h.mux()
        .create_tab("ark-contract-inside", "builder", Path::new("/tmp/x.kdl"))
        .await
        .expect("inside-zellij first tab must succeed");
    let calls = h.recorded_calls();
    assert!(calls.iter().any(|c| c.argv_contains("switch-session")));
    assert!(calls.iter().any(|c| c.argv_contains("--layout")));
    for c in &calls {
        assert!(
            !c.argv_contains("--create"),
            "switch-session must never carry --create (issue: that flag \
             only exists on `attach`); offending: {} {:?}",
            c.program,
            c.args
        );
    }
}

/// Zellij-specific scenario: KDL layout rendering + `{{cwd}}`
/// interpolation. Covers cavekit-mux-zellij.md R5 / cavekit-layouts.md
/// R3. The template surface is bounded to five vars; this test exercises
/// the `cwd` and `name` substitutions and confirms the validator
/// accepts the output.
#[test]
fn zellij_kdl_layout_renders_cwd_interpolation() {
    let tmpl = r#"layout {
    cwd "{{ cwd }}"
    tab name="{{ name }}" {
        pane command="{{ agent_cmd }}"
    }
}
"#;
    let vars = LayoutVars {
        cwd: "/home/contract/worktree".to_string(),
        agent_cmd: "claude".to_string(),
        agent_args: Vec::new(),
        id: "contract-id".to_string(),
        name: "builder".to_string(),
    };
    let rendered = render_layout(tmpl, &vars).expect("KDL template must render");
    assert!(
        rendered.contains("cwd \"/home/contract/worktree\""),
        "rendered KDL must carry interpolated cwd; got:\n{rendered}"
    );
    assert!(
        rendered.contains("name=\"builder\""),
        "rendered KDL must carry interpolated tab name; got:\n{rendered}"
    );
    assert!(
        rendered.contains("command=\"claude\""),
        "rendered KDL must carry interpolated agent_cmd; got:\n{rendered}"
    );
}

/// Zellij-specific scenario: templates with undefined vars MUST fail
/// (cavekit-mux-zellij.md R5 — "Rendering validates KDL syntax before
/// calling zellij (reject malformed with clear error)"). Strict
/// undefined-var behavior is the contract-suite-level way to catch
/// silent variable drift.
#[test]
fn zellij_kdl_layout_rejects_undefined_vars() {
    let tmpl = "layout {\n    cwd \"{{ not_a_real_var }}\"\n}\n";
    let vars = LayoutVars {
        cwd: "/tmp".to_string(),
        agent_cmd: "x".to_string(),
        agent_args: Vec::new(),
        id: "id".to_string(),
        name: "n".to_string(),
    };
    let err = render_layout(tmpl, &vars).expect_err("undefined template var must produce an error");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("undefined") || msg.contains("not_a_real_var"),
        "expected undefined-var error, got: {msg}"
    );
}
