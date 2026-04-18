//! T-017: verify ark-view types flow through ark-ext-proto so
//! extension authors import from one crate.

use ark_ext_proto::{
    CommandView, HandleId, HandleKind, InvalidationCause, Pane, PaneLike, Stack, TabHandle, View,
    ZellijView,
};

struct MyCmdView;
impl View for MyCmdView {}
impl CommandView for MyCmdView {}

struct MyZellijView;
impl View for MyZellijView {}
impl ZellijView for MyZellijView {}

#[test]
fn reexported_types_are_usable_from_ark_ext_proto() {
    // All these names must resolve through ark_ext_proto — if the
    // re-exports drift, this file stops compiling.
    let _k: HandleKind = HandleKind::Pane;
    let _id: HandleId = HandleId::new("test");
    let _c: InvalidationCause = InvalidationCause::UserClosed;
    let _ = MyCmdView;
    let _ = MyZellijView;
    // Cannot construct Pane/Stack/TabHandle directly (crate-private
    // ctors); reference the types by path to ensure import resolves.
    fn _accepts_pane_like<P: PaneLike>(_p: &P) {}
    fn _names_pane<V: View>() -> std::marker::PhantomData<Pane<V>> {
        std::marker::PhantomData
    }
    fn _names_stack<V: View>() -> std::marker::PhantomData<Stack<V>> {
        std::marker::PhantomData
    }
    fn _names_tab() -> std::marker::PhantomData<TabHandle> {
        std::marker::PhantomData
    }
    let _ = _names_pane::<MyCmdView>();
    let _ = _names_stack::<MyCmdView>();
    let _ = _names_tab();
}
