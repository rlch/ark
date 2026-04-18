//! View trait trio. All three are pure markers (no required methods).
//! `View` is the base contract every scene-usable type must satisfy;
//! `CommandView` + `ZellijView` refine it to two mutually-exclusive
//! render modes. Extension-defined refinements (e.g. `trait DiffView:
//! View {}`) can layer on top to gate intents on capability groups.
//!
//! Per scene R17 + cavekit-soul-phase-2-ark-view.md R3. Affordance
//! methods (`env`, `write_stdin`, `pid`, `pipe`) land on `Pane<V>`
//! in inherent impl blocks gated by these markers — see R4.

/// Base marker every scene-usable view must implement.
///
/// `Send + Sync + 'static` bound is deliberate: extension code passes
/// views across async task boundaries, so non-Send types cannot serve
/// as scene views. No required methods — `View` is pure type-level
/// classification.
pub trait View: Send + Sync + 'static {}

/// Refinement marker for command-backed views — i.e. views whose
/// renderer is a subprocess ark spawns (editors, REPLs, TUI agents,
/// engine clients).
///
/// `Pane<V: CommandView>` gains inherent methods for the
/// process-shaped surface: `env(&mut self, …)`, `write_stdin(…)`,
/// `pid() -> …` (see R4).
pub trait CommandView: View {}

/// Refinement marker for zellij-plugin-backed views — i.e. views
/// whose renderer is a wasm plugin zellij loads.
///
/// `Pane<V: ZellijView>` gains inherent methods for the plugin-shaped
/// surface: `pipe(…)` (see R4).
pub trait ZellijView: View {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_has_no_required_methods() {
        struct Empty;
        impl View for Empty {}
        let _: Empty = Empty;
    }

    #[test]
    fn command_view_has_no_required_methods_beyond_view() {
        #[allow(dead_code)] // trait-impl existence is the assertion
        struct X;
        impl View for X {}
        impl CommandView for X {}
    }

    #[test]
    fn zellij_view_has_no_required_methods_beyond_view() {
        #[allow(dead_code)] // trait-impl existence is the assertion
        struct Y;
        impl View for Y {}
        impl ZellijView for Y {}
    }

    #[test]
    fn downstream_trait_can_refine_view() {
        #[allow(dead_code)] // trait-impl existence is the assertion
        trait DiffView: View {}
        #[allow(dead_code)]
        struct Z;
        impl View for Z {}
        impl DiffView for Z {}
    }

    #[test]
    fn view_is_send_sync_static() {
        #[allow(dead_code)] // bound check via instantiation below
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        fn _check<V: View>() {
            assert_send_sync_static::<V>()
        }
    }

    #[test]
    fn view_can_be_boxed_as_trait_object_with_send_sync() {
        struct A;
        impl View for A {}
        let _b: Box<dyn View + Send + Sync> = Box::new(A);
    }
}
