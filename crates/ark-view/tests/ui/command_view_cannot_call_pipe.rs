//! Negative fixture: `pipe()` is gated on `V: ZellijView`. A view
//! that only implements `CommandView` MUST NOT see the method.

use ark_view::{CommandView, HandleId, Pane, View, __trybuild_pane_ctor};

struct CVOnly;
impl View for CVOnly {}
impl CommandView for CVOnly {}

fn main() {
    let p: Pane<CVOnly> = __trybuild_pane_ctor::<CVOnly>(HandleId::new("x"));
    // pipe() requires V: ZellijView — must NOT be in scope:
    p.pipe(b"msg");
}
