//! T-009+T-010+T-011 (cavekit R4): compile-fail fixtures that lock
//! marker-trait gating on `Pane<V>`'s affordance methods.
//!
//! A `Pane<V>` where `V: ZellijView` (and not `CommandView`) MUST NOT
//! see `env`/`write_stdin`/`pid` — those are gated on `CommandView`.
//! Symmetrically, a `Pane<V>` where `V: CommandView` (and not
//! `ZellijView`) MUST NOT see `pipe`.
//!
//! If any of the four `.rs` fixtures under `tests/ui/` ever compiles
//! successfully, the marker-gating invariant is broken and this test
//! fails — the whole point of the negative matrix.

#[test]
fn command_view_methods_unreachable_for_zellij_only_view() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/zellij_view_cannot_call_env.rs");
    t.compile_fail("tests/ui/zellij_view_cannot_call_write_stdin.rs");
    t.compile_fail("tests/ui/zellij_view_cannot_call_pid.rs");
    t.compile_fail("tests/ui/command_view_cannot_call_pipe.rs");
}
