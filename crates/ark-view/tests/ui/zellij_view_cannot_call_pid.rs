//! Negative fixture: `pid()` is gated on `V: CommandView`. A view
//! that only implements `ZellijView` MUST NOT see the method.

use ark_view::{HandleId, Pane, View, ZellijView, __trybuild_pane_ctor};

struct ZVOnly;
impl View for ZVOnly {}
impl ZellijView for ZVOnly {}

fn main() {
    let p: Pane<ZVOnly> = __trybuild_pane_ctor::<ZVOnly>(HandleId::new("x"));
    // pid() requires V: CommandView — must NOT be in scope:
    let _ = p.pid();
}
