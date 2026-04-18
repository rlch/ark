//! Negative fixture: `env()` is gated on `V: CommandView`. A view
//! that only implements `ZellijView` MUST NOT see the method.

use ark_view::{CommandView, HandleId, Pane, View, ZellijView, __trybuild_pane_ctor};

struct ZVOnly;
impl View for ZVOnly {}
impl ZellijView for ZVOnly {}

// Prove ZVOnly does NOT implement CommandView — purely for human
// reviewers; the compile-fail below is what the test actually checks.
fn _assert_not_command_view<T: CommandView>() {}

fn main() {
    let p: Pane<ZVOnly> = __trybuild_pane_ctor::<ZVOnly>(HandleId::new("x"));
    // env() requires V: CommandView — must NOT be in scope:
    p.env("K", "V");
}
