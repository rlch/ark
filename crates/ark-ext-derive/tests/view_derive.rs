//! T-025 (build-site-soul-phase-2.md): `#[derive(View)]` +
//! `#[ark_view(name = "…")]` attribute family.
//!
//! # What this file pins
//!
//! 1. The base-case derive compiles and auto-derives the view name
//!    from the struct's PascalCase identifier
//!    (`MyPanel` → `"my-panel"`).
//! 2. `#[ark_view(name = "custom")]` overrides the auto-derived name.
//! 3. The `inventory::submit!` block expanded by the derive registers
//!    one `ViewRegistration` per derived struct — traversable via
//!    `inventory::iter::<ViewRegistration>` at test time.
//!
//! Mirrors the shape of `#[ark_intent]`: the name is lowercase-kebab,
//! the struct's type name is stamped into `component`, and the record
//! is wired through `inventory` so the scene compiler can collect
//! submissions at startup without manual registration.

use ark_ext_derive::View;
use ark_ext_metadata_types::ViewRegistration;

/// Base-case derive — auto-name from struct (`"my-panel"`).
#[derive(View)]
#[allow(dead_code)]
struct MyPanel;

/// `#[ark_view(name = "…")]` override.
#[derive(View)]
#[ark_view(name = "custom-name")]
#[allow(dead_code)]
struct OverrideName;

/// Multi-word PascalCase struct name auto-derives to multi-segment
/// kebab-case (`"git-status-view"`).
#[derive(View)]
#[allow(dead_code)]
struct GitStatusView;

/// `description` field is forwarded into the submitted record.
#[derive(View)]
#[ark_view(name = "with-desc", description = "test description")]
#[allow(dead_code)]
struct WithDescription;

#[test]
fn derive_view_compiles() {
    // Compile-gate: if the derive is missing or the attribute name
    // changed incompatibly, this file fails to compile. Running this
    // test is the assertion.
    let _ = MyPanel;
    let _ = OverrideName;
    let _ = GitStatusView;
    let _ = WithDescription;
}

#[test]
fn derive_view_auto_derives_name_from_struct() {
    // The base-case derive (no `#[ark_view(name = …)]`) should stamp
    // a record whose `name` is the struct's PascalCase identifier
    // lowered to kebab-case.
    let reg = find_registration("MyPanel").expect("MyPanel registration missing");
    assert_eq!(reg.name, "my-panel");
    assert_eq!(reg.component, "MyPanel");
    assert_eq!(reg.description, "");
}

#[test]
fn derive_view_override_name() {
    // `#[ark_view(name = "custom-name")]` overrides the auto-derived
    // name while leaving `component` set to the struct's type name.
    let reg = find_registration("OverrideName").expect("OverrideName registration missing");
    assert_eq!(reg.name, "custom-name");
    assert_eq!(reg.component, "OverrideName");
}

#[test]
fn derive_view_multi_word_struct_name() {
    // Multi-word PascalCase splits on every uppercase boundary.
    let reg = find_registration("GitStatusView").expect("GitStatusView registration missing");
    assert_eq!(reg.name, "git-status-view");
}

#[test]
fn derive_view_forwards_description() {
    // `description` attribute lands in the submitted record verbatim.
    let reg = find_registration("WithDescription").expect("WithDescription registration missing");
    assert_eq!(reg.name, "with-desc");
    assert_eq!(reg.description, "test description");
}

/// Walk the `inventory` collector for `ViewRegistration` and return the
/// record whose `component` matches `type_name`. Returns `None` if no
/// matching record was submitted — which in this test setup means the
/// derive failed to emit the `inventory::submit!` block.
fn find_registration(type_name: &str) -> Option<&'static ViewRegistration> {
    inventory::iter::<ViewRegistration>
        .into_iter()
        .find(|reg| reg.component == type_name)
}
