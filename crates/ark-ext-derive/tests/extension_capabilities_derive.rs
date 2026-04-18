//! T-027 (build-site-soul-phase-2.md): auto-capability-advertisement
//! via `#[extension(capabilities = "...")]` on `#[derive(Extension)]`
//! (kit cavekit-soul-phase-2-ext-surface.md R7).
//!
//! # What this file pins
//!
//! 1. `#[extension(capabilities = "flag1,flag2")]` stamps an inherent
//!    const `Self::ARK_CAPABILITIES: &'static [&'static str]` on the
//!    annotated type.
//! 2. Omitting the attribute yields an empty const (not absent —
//!    downstream host-dispatch reads it unconditionally).
//! 3. Whitespace around separators is trimmed; empty entries are
//!    dropped.
//! 4. Plain `#[derive(Extension)]` without the attribute continues to
//!    compile + registers an `ExtensionMeta` record as before
//!    (additive-only change).
//!
//! # Scope (PATH: manual-override attribute + caveat doc)
//!
//! Per kit R7: proc macros cannot see sibling `impl ArkExtension for T
//! { ... }` blocks at macro-expansion time, so we cannot auto-detect
//! which lifecycle methods the author overrode. The manual-override
//! path here is the "convenience, not a gate" surface the kit calls
//! out. Extensions whose capabilities cannot be captured by this
//! attribute continue to declare them in `extension.kdl` by hand.

use ark_ext_derive::Extension;
use ark_ext_metadata_types::ExtensionMeta;

/// Baseline: pre-T-027 shape continues to compile + register.
#[derive(Extension)]
#[extension(name = "no-caps-ext", version = "0.1.0")]
#[allow(dead_code)]
struct NoCapsExt;

/// Single capability flag.
#[derive(Extension)]
#[extension(
    name = "single-cap-ext",
    version = "0.1.0",
    capabilities = "view.pane.v1"
)]
#[allow(dead_code)]
struct SingleCapExt;

/// Multiple capability flags with varied whitespace around separators
/// to exercise the trim + filter logic.
#[derive(Extension)]
#[extension(
    name = "multi-cap-ext",
    version = "0.1.0",
    capabilities = "view.pane.v1, ext.lifecycle.v1 ,view.stack.v1"
)]
#[allow(dead_code)]
struct MultiCapExt;

/// Edge case: empty entries / trailing commas filtered out.
#[derive(Extension)]
#[extension(
    name = "edge-cap-ext",
    version = "0.1.0",
    capabilities = "view.pane.v1,,ext.doctor.v1,"
)]
#[allow(dead_code)]
struct EdgeCapExt;

#[test]
fn no_capabilities_yields_empty_const() {
    assert_eq!(NoCapsExt::ARK_CAPABILITIES, &[] as &[&str]);
}

#[test]
fn single_capability_flag_is_stamped() {
    assert_eq!(SingleCapExt::ARK_CAPABILITIES, &["view.pane.v1"]);
}

#[test]
fn multi_capability_flags_preserve_declared_order() {
    assert_eq!(
        MultiCapExt::ARK_CAPABILITIES,
        &["view.pane.v1", "ext.lifecycle.v1", "view.stack.v1"],
    );
}

#[test]
fn empty_entries_and_whitespace_are_filtered() {
    assert_eq!(
        EdgeCapExt::ARK_CAPABILITIES,
        &["view.pane.v1", "ext.doctor.v1"],
    );
}

#[test]
fn extension_meta_registration_unaffected_by_capabilities_attr() {
    // T-027 is additive — existing `ExtensionMeta` inventory stream
    // still carries all four registrations. Asserting by name ensures
    // we didn't accidentally swap the inventory submission for the
    // const.
    let found: std::collections::BTreeSet<&'static str> = inventory::iter::<ExtensionMeta>
        .into_iter()
        .map(|m| m.name)
        .collect();
    for name in [
        "no-caps-ext",
        "single-cap-ext",
        "multi-cap-ext",
        "edge-cap-ext",
    ] {
        assert!(
            found.contains(name),
            "ExtensionMeta inventory missing {name}; got: {found:?}"
        );
    }
}
