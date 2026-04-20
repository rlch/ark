//! T-PP-034 (cavekit-plugin-protocol R4): `LinkerSet` integration test.
//!
//! Verifies:
//! * `LinkerSet::build` with two distinct cap permutations produces
//!   three variants (two requested + always-present `empty`).
//! * Every requested `CapsKey` is reachable via `for_caps`.
//! * `for_caps(&CapsKey::new())` always returns `Some(_)`.
//! * A key that was not declared at build time resolves to `None` —
//!   no implicit fallback to `empty`.

use std::collections::BTreeSet;

use ark_host::{CapsKey, LinkerSet};

fn key_of(caps: &[&str]) -> CapsKey {
    caps.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
}

#[test]
fn builds_distinct_linkers_for_distinct_permutations() {
    let fs_only = key_of(&["fs-read"]);
    let net_plus_fs = key_of(&["fs-read", "network"]);

    let set =
        LinkerSet::build(vec![fs_only.clone(), net_plus_fs.clone()]).expect("LinkerSet::build");

    // Three variants total: fs_only, net_plus_fs, empty.
    assert_eq!(
        set.variant_count(),
        3,
        "expected 3 variants (2 declared + 1 empty); got {}",
        set.variant_count()
    );

    // Both declared keys resolve.
    assert!(set.for_caps(&fs_only).is_some(), "fs-read variant missing");
    assert!(
        set.for_caps(&net_plus_fs).is_some(),
        "network+fs-read variant missing"
    );
    // Different permutations of the same cap-set collapse to one key —
    // `CapsKey = BTreeSet<String>` so insertion order doesn't matter.
    let same_key_reordered = key_of(&["network", "fs-read"]);
    assert!(
        set.for_caps(&same_key_reordered).is_some(),
        "BTreeSet CapsKey must be permutation-insensitive"
    );
}

#[test]
fn empty_variant_is_always_present() {
    // Build with zero declared cap-sets — the empty variant must still
    // exist because a zero-grant plugin runs against it.
    let set = LinkerSet::build(vec![]).expect("LinkerSet::build zero-input");
    let empty = CapsKey::new();
    assert!(
        set.for_caps(&empty).is_some(),
        "LinkerSet must always contain the `empty` CapsKey variant"
    );
    // And variant_count is exactly 1 — just the empty variant.
    assert_eq!(set.variant_count(), 1);
}

#[test]
fn empty_variant_is_present_even_when_declared_sets_are_non_empty() {
    // Two non-empty declared sets — empty variant still shows up.
    let a = key_of(&["fs-read"]);
    let b = key_of(&["fs-write"]);
    let set = LinkerSet::build(vec![a, b]).expect("LinkerSet::build");
    assert!(
        set.for_caps(&CapsKey::new()).is_some(),
        "empty variant must be present regardless of declared cap-sets"
    );
}

#[test]
fn undeclared_caps_key_returns_none() {
    let declared = key_of(&["fs-read"]);
    let set = LinkerSet::build(vec![declared]).expect("LinkerSet::build");
    // A key that was NOT declared at build time must not match — no
    // implicit fallback. Callers are expected to validate grants
    // against the declared set at KDL-parse time (kit R5).
    let undeclared = key_of(&["network", "spawn-process"]);
    assert!(
        set.for_caps(&undeclared).is_none(),
        "undeclared CapsKey must NOT resolve to a linker"
    );
}

#[test]
fn duplicate_declared_sets_collapse_to_one_variant() {
    // Two identical declared sets — we still get 2 variants total
    // (the one unique set + the always-present empty variant).
    let fs = key_of(&["fs-read"]);
    let set = LinkerSet::build(vec![fs.clone(), fs]).expect("LinkerSet::build");
    assert_eq!(
        set.variant_count(),
        2,
        "duplicate declared CapsKeys must collapse; expected 2, got {}",
        set.variant_count()
    );
}
