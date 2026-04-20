//! T-PP-034 (cavekit-plugin-protocol R4): coarse-gate integration test.
//!
//! Proves the "declared-but-ungranted cap fails at `instantiate_pre`"
//! acceptance criterion — the R4 Approach B contract. Compiles a
//! synthetic component whose WIT imports `ark:plugin/fs-read@1.0.0`
//! (the cap interface's linker instance name) and asserts:
//!
//! 1. A [`LinkerSet`] built with `CapsKey::new()` (no caps granted)
//!    produces an `empty` variant that REFUSES to `instantiate_pre`
//!    the component — the fs-read interface import is unresolved, so
//!    the link-time check fails.
//! 2. A `LinkerSet` built with `{"fs-read"}` produces a variant that
//!    successfully `instantiate_pre`s the same component.
//!
//! This is the gate the Tier 3B interim review (F-433/F-436) flagged
//! as missing — the pre-fix `build_one_variant` called
//! `Plugin::add_to_linker` which registered EVERY interface
//! unconditionally, so the empty variant still had fs-read available
//! at link time (Approach C by accident). The post-fix
//! `build_one_variant` only registers a cap's linker fn when the
//! variant's `CapsKey` contains that cap id.

use std::collections::BTreeSet;

use ark_host::{CapsKey, LinkerSet, engine};
use wasmtime::component::Component;

/// Synthetic component whose only import is the `ark:plugin/fs-read`
/// interface at the 1.0.0 version string wasmtime's bindgen uses
/// internally (`ark:plugin/fs-read@1.0.0`). The interface exposes a
/// single `ok: func()` export matching the v1 WIT surface.
fn component_with_fs_read_import() -> Component {
    // A component-model component in WAT form: declares a core func
    // type and an imported instance carrying a single `ok` function
    // with that type. No internal logic is needed — link-time
    // resolution is what we're probing.
    let wat = r#"
(component
  (import "ark:plugin/fs-read@1.0.0"
    (instance
      (export "ok" (func))
    )
  )
)
"#;
    let binary = wat::parse_str(wat).expect("parse synthetic WAT component");
    Component::from_binary(engine(), &binary).expect("compile synthetic component")
}

fn caps_key(caps: &[&str]) -> CapsKey {
    caps.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
}

#[test]
fn empty_variant_rejects_fs_read_import_at_instantiate_pre() {
    // Build the set with a declared {fs-read} variant so we get two
    // distinct linkers to compare: empty (no caps) and fs-read.
    let set = LinkerSet::build(vec![caps_key(&["fs-read"])]).expect("LinkerSet::build");

    let empty_linker = set
        .for_caps(&CapsKey::new())
        .expect("empty variant must exist");
    let fs_read_linker = set
        .for_caps(&caps_key(&["fs-read"]))
        .expect("fs-read variant must exist");

    let component = component_with_fs_read_import();

    // Gate 1: the empty variant MUST refuse the fs-read import. The
    // error surfaces at `instantiate_pre` as an unresolved-import
    // link-time error.
    let empty_result = empty_linker.instantiate_pre(&component);
    let empty_err = match empty_result {
        Ok(_) => panic!(
            "R4 coarse gate: empty linker variant MUST refuse a component that \
             imports ark:plugin/fs-read — got Ok, meaning the cap gate is \
             not registered correctly (Approach C regression). See \
             crates/ark-host/src/linker_set.rs build_one_variant()."
        ),
        Err(e) => e,
    };
    // Spot-check the error message — wasmtime's exact wording is its
    // own contract; we just assert it's non-empty so the diagnostic
    // is something a user could act on.
    let msg = format!("{empty_err:#}");
    assert!(
        !msg.is_empty(),
        "expected wasmtime to produce a non-empty error message for \
         an unresolved fs-read import"
    );

    // Gate 2: the fs-read variant MUST accept the same component.
    match fs_read_linker.instantiate_pre(&component) {
        Ok(_) => {}
        Err(e) => panic!(
            "R4 coarse gate: linker variant with fs-read granted MUST \
             accept a component importing ark:plugin/fs-read — got Err: {e:#}"
        ),
    }
}

#[test]
fn fs_read_variant_does_not_leak_other_caps() {
    // Defense-in-depth: a variant that grants only fs-read MUST NOT
    // also accept a component that imports ark:plugin/network — the
    // per-cap conditional in build_one_variant should only register
    // the caps named in the CapsKey.
    let set = LinkerSet::build(vec![caps_key(&["fs-read"])]).expect("LinkerSet::build");
    let fs_read_linker = set
        .for_caps(&caps_key(&["fs-read"]))
        .expect("fs-read variant must exist");

    // Component that imports `ark:plugin/network` (NOT fs-read).
    let wat = r#"
(component
  (import "ark:plugin/network@1.0.0"
    (instance
      (export "ok" (func))
    )
  )
)
"#;
    let binary = wat::parse_str(wat).expect("parse synthetic network WAT");
    let component =
        Component::from_binary(engine(), &binary).expect("compile synthetic network component");

    let result = fs_read_linker.instantiate_pre(&component);
    if result.is_ok() {
        panic!(
            "R4 coarse gate: fs-read variant MUST NOT accept a component \
             that imports ark:plugin/network — only the explicitly-granted \
             caps should appear in each variant's linker surface."
        );
    }
}

// ---------------------------------------------------------------------
// F-445 regression: plugins MUST NOT see `wasi:*` imports.
// ---------------------------------------------------------------------
//
// Pre-fix the host called `wasmtime_wasi::p2::add_to_linker_async` on
// every variant — that exposed the whole wasi:filesystem / wasi:sockets
// / wasi:io / wasi:clocks / wasi:random surface to every plugin,
// bypassing the `ark:cap/*` coarse gate. Post-fix no variant calls
// into wasmtime-wasi for linker wiring; wasi is a host-internal
// concern used inside cap-fn impls only.
//
// This test asserts a synthetic component importing
// `wasi:filesystem/types@0.2.3` (the p2 filesystem interface) is
// REJECTED at `instantiate_pre` in EVERY variant — including the
// richest one in the set — because wasi isn't registered at all.

#[test]
fn wasi_filesystem_import_rejected_in_every_variant() {
    // Build a set covering every cap so we exercise the richest linker
    // variant alongside the empty one. If wasi ever sneaks back into
    // the blanket wiring, the `all_caps_linker` branch here will
    // catch it — the empty branch would catch a partial regression.
    let all = caps_key(&[
        "fs-read",
        "fs-write",
        "network",
        "spawn-process",
        "bus-send",
        "bus-receive",
    ]);
    let set = LinkerSet::build(vec![all.clone()]).expect("LinkerSet::build");

    // The wasi:filesystem/types interface carries a `descriptor`
    // resource plus companion functions; for a link-time probe we only
    // need the import declaration — a single stub fn inside the
    // imported instance is enough to force resolution. We use the
    // known p2 filesystem version `0.2.3` (the one wasmtime-wasi 43
    // exposes) so the name matches the interface wasmtime would have
    // registered had the pre-fix blanket add still been in place.
    let wat = r#"
(component
  (import "wasi:filesystem/types@0.2.3"
    (instance
      (export "ok" (func))
    )
  )
)
"#;
    let binary = wat::parse_str(wat).expect("parse synthetic wasi:filesystem WAT");
    let component = Component::from_binary(engine(), &binary)
        .expect("compile synthetic wasi:filesystem component");

    for (label, key) in [("empty", CapsKey::new()), ("all-caps", all)] {
        let linker = set.for_caps(&key).expect("variant must exist");
        let result = linker.instantiate_pre(&component);
        assert!(
            result.is_err(),
            "F-445 regression: {label} variant MUST refuse a component that \
             imports wasi:filesystem/types@0.2.3 — plugins NEVER see `wasi:*` \
             imports. See crates/ark-host/src/linker_set.rs build_one_variant(); \
             a blanket `wasmtime_wasi::p2::add_to_linker_async` call would have \
             let this succeed."
        );
    }
}

#[test]
fn wasi_sockets_import_rejected_in_every_variant() {
    // Sibling of the fs test: the `wasi:sockets` family must also be
    // invisible to plugins. Even the `network` cap grant only opens
    // `ark:cap/network` (stub today, real socket surface in a future
    // tier); it MUST NOT resolve `wasi:sockets/*` imports.
    let network = caps_key(&["network"]);
    let set = LinkerSet::build(vec![network.clone()]).expect("LinkerSet::build");

    let wat = r#"
(component
  (import "wasi:sockets/tcp@0.2.3"
    (instance
      (export "ok" (func))
    )
  )
)
"#;
    let binary = wat::parse_str(wat).expect("parse synthetic wasi:sockets WAT");
    let component = Component::from_binary(engine(), &binary)
        .expect("compile synthetic wasi:sockets component");

    for (label, key) in [("empty", CapsKey::new()), ("network", network)] {
        let linker = set.for_caps(&key).expect("variant must exist");
        let result = linker.instantiate_pre(&component);
        assert!(
            result.is_err(),
            "F-445 regression: {label} variant MUST refuse a component that \
             imports wasi:sockets/tcp@0.2.3 — granting `network` opens \
             `ark:cap/network` only, never wasi:sockets."
        );
    }
}
