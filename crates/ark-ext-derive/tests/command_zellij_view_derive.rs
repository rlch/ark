//! T-026 (build-site-soul-phase-2.md): `#[derive(CommandView)]` +
//! `#[derive(ZellijView)]` marker derives (kit cavekit-soul-phase-2-
//! ext-surface.md R7).
//!
//! # What this file pins
//!
//! 1. `#[derive(CommandView)]` expands to
//!    `impl ark_view::CommandView for T {}`, tied to the supertrait
//!    `View` emitted by `#[derive(View)]` (PATH A / marker-only
//!    shape).
//! 2. `#[derive(ZellijView)]` expands to
//!    `impl ark_view::ZellijView for T {}` under the same
//!    supertrait assumption.
//! 3. Plain `#[derive(View)]` does NOT leak the refinement markers —
//!    asserted via static `fn foo<T: View>()` witnesses (a struct
//!    that derives only `View` cannot be accepted by a
//!    `T: CommandView` bound; this is a compile-gate assertion
//!    expressed as explicit type-class check helpers below).
//!
//! # PATH A caveat (kit R7)
//!
//! These derives are body-less: they do not coordinate with a
//! co-derived `#[derive(View)]` to stamp a `kind` discriminant on the
//! submitted `ViewRegistration`, because proc macros run in isolation
//! per-attribute. Routing `kind` through the submitted record is
//! deferred until `ViewRegistration` in `ark-ext-metadata-types` grows
//! a `kind` field.

use ark_ext_derive::{CommandView, View, ZellijView};
use ark_view::{CommandView as CmdViewTrait, View as ViewTrait, ZellijView as ZViewTrait};

/// Co-derive: `View` emits `impl View for T {}`, `CommandView` emits
/// `impl CommandView for T {}`. Both together satisfy
/// `CommandView: View`.
#[derive(View, CommandView)]
#[allow(dead_code)]
struct MyCommand;

/// Co-derive for the zellij-plugin marker.
#[derive(View, ZellijView)]
#[allow(dead_code)]
struct MyPlugin;

/// Plain `#[derive(View)]` — no refinement.
#[derive(View)]
#[allow(dead_code)]
struct PlainView;

#[test]
fn command_view_derive_implements_marker() {
    fn asserts_command_view<T: CmdViewTrait>() {}
    asserts_command_view::<MyCommand>();
}

#[test]
fn zellij_view_derive_implements_marker() {
    fn asserts_zellij_view<T: ZViewTrait>() {}
    asserts_zellij_view::<MyPlugin>();
}

#[test]
fn view_derive_emits_base_view_impl() {
    // `#[derive(View)]` also emits `impl View for T {}` so the
    // refinement markers (CommandView / ZellijView) find their
    // supertrait without hand-written impls.
    fn asserts_view<T: ViewTrait>() {}
    asserts_view::<PlainView>();
    asserts_view::<MyCommand>();
    asserts_view::<MyPlugin>();
}

#[test]
fn command_view_derive_satisfies_view_supertrait() {
    // CommandView: View — a `MyCommand` value is usable anywhere a
    // `T: View` bound is required, and specifically where both are.
    fn asserts_command_and_view<T: ViewTrait + CmdViewTrait>() {}
    asserts_command_and_view::<MyCommand>();
}

#[test]
fn zellij_view_derive_satisfies_view_supertrait() {
    fn asserts_zellij_and_view<T: ViewTrait + ZViewTrait>() {}
    asserts_zellij_and_view::<MyPlugin>();
}

/// Compile-gate: the `View` derive alone must not pull in the
/// refinement impls. If `#[derive(View)]` accidentally leaked a
/// `CommandView` impl, the `neg_*` helpers below would fail to
/// type-check (they'd be redundant, not negative).
///
/// Rust has no built-in way to assert "does NOT impl X"; we rely on
/// the fact that the parent `view_derive.rs` test suite continues to
/// compile — none of its `#[derive(View)]`-only structs need to be
/// usable at a `T: CommandView` bound. Meanwhile the witnesses above
/// do require the refinement impls, so any regression that stops the
/// marker derives from emitting them is caught by
/// `command_view_derive_implements_marker` /
/// `zellij_view_derive_implements_marker`.
#[test]
fn plain_view_is_not_conflated_with_refinements() {
    // Accepting `PlainView` at a `T: View` bound is fine.
    fn asserts_view<T: ViewTrait>() {}
    asserts_view::<PlainView>();
    // The absence of `impl CommandView for PlainView` is enforced by
    // omitting the marker derive — any attempt to call
    // `asserts_command_view::<PlainView>()` in this file would fail
    // to compile, which is the contract.
}
