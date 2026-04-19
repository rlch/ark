//! Harness self-test.
//!
//! Exercises [`ArkHarness`] at a basic level. The test is
//! intentionally defensive: it SKIPs cleanly when the environment
//! can't run a zellij launch and uses loose timeouts so it won't
//! regress CI.
//!
//! Scope:
//!   * `try_new` returns `Some` when zellij is on PATH and we're not
//!     nested, `None` otherwise.
//!   * When `Some`, `wait_for_ready` resolves before `shutdown` cleans up.
//!
//! Deeper assertions (pane count, view rendering) are deferred to
//! downstream integration tests — see v0.2-backlog.md #6 ledger for
//! the rationale around MVP scope.

use std::path::PathBuf;
use std::time::Duration;

use ark_test_harness::{ArkHarness, discover_workspace_binary, harness_can_run};

/// Resolve the ark binary for this harness self-test. Since
/// `CARGO_BIN_EXE_*` is package-scoped and this crate doesn't own
/// `ark`, we discover it via the workspace `target/debug/` directory.
///
/// Returns `None` when the binary isn't built yet — the smoke test
/// SKIPs in that case instead of failing, so `cargo test -p
/// ark-test-harness` remains robust even before `cargo build` has
/// touched the ark crate.
fn discover_ark_bin() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    discover_workspace_binary(&manifest, "ark")
}

#[test]
fn harness_skip_branch_is_advertised() {
    // Sanity: the `harness_can_run` predicate exists and returns
    // without panicking. Exact value is environment-dependent.
    let _ = harness_can_run();
}

#[test]
fn try_new_yields_none_when_environment_unsuitable() {
    // When zellij is off PATH OR we're inside zellij, try_new must
    // return Ok(None) — the SKIP-contract for downstream callers.
    if harness_can_run() {
        eprintln!("SKIP: environment IS suitable; this test covers the unsuitable branch only");
        return;
    }
    // Any path works here because the skip check runs first.
    let dummy = PathBuf::from("/bin/true");
    let result = ArkHarness::try_new(dummy, "layout {}").expect("try_new must not error on skip");
    assert!(
        result.is_none(),
        "unsuitable environment must yield Ok(None)"
    );
}

/// Ark-under-zellij launch test. Runs only when zellij is on PATH,
/// we're outside an existing zellij session, AND the ark binary has
/// been built.
///
/// Intentionally uses the default-scene rather than a crafted KDL —
/// we're testing the harness plumbing, not scene compilation.
#[test]
fn try_new_launches_ark_under_zellij_when_possible() {
    if !harness_can_run() {
        eprintln!("SKIP: zellij missing or nested — harness self-test cannot run");
        return;
    }
    let Some(ark_bin) = discover_ark_bin() else {
        eprintln!(
            "SKIP: ark binary not found under target/debug — run `cargo build -p ark-cli` first"
        );
        return;
    };

    // Use the default-scene (empty `--scene` value writes out the
    // embedded default), not a crafted KDL — keeps the harness
    // self-test decoupled from scene DSL evolution.
    let scene = ark_test_harness::fixtures::MINIMAL_SCENE_KDL;

    let Some(harness) =
        ArkHarness::try_new(&ark_bin, scene).expect("harness construction should not error")
    else {
        // Double-checked: `harness_can_run` returned true above, but
        // `build` can still skip if a race surface removed zellij.
        // Treat as SKIP rather than panic.
        eprintln!("SKIP: HarnessBuilder::build reported None despite can_run=true");
        return;
    };

    // Generous timeout — real fork + zellij session bring-up is the
    // slowest path. `launch_pty.rs` uses 10s; we mirror that.
    let ready = harness.wait_for_ready(Duration::from_secs(10));

    // Regardless of readiness outcome, always shut down cleanly to
    // avoid leaking a zellij session across test runs.
    let session_name = harness.session_name().to_string();
    let pty_tail = harness.pty_buffer();
    harness.shutdown().expect("shutdown should not error");

    ready.unwrap_or_else(|e| {
        panic!(
            "harness never became ready for session `{session_name}`: {e}\npty tail:\n{pty_tail}"
        )
    });
}
