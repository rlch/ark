//! T-041 (cavekit-soul-phase-2-tests.md R5): trybuild compile-fail +
//! compile-pass goldens for the view-type symbol-table surface
//! (T-034 `ViewTypeTable` + `validate_view_reference`) PLUS the
//! KDL-level `validate_scene!` proc-macro validator.
//!
//! # Scope
//!
//! Two layers of fixtures cohabit under `tests/ui/`:
//!
//! ## Rust-level (pre-R5)
//!
//! Pin the raw struct + runtime-API surface:
//!
//! - `view_decl_wrong_field_type.rs` — rustc E0308 guards `ViewDecl.name`
//!   against a silent type-relax.
//! - `metadata_missing_views_field.rs` — rustc E0063 guards
//!   `ExtensionMetadata` struct-literal callers against silent field
//!   removal.
//! - `valid_pane_and_stack_decls.rs` — compile-pass round-trip through
//!   `ViewTypeTable::from_manifests` + `validate_view_reference`.
//! - `cross_ext_view_reference.rs` — compile-pass cross-ext namespaced
//!   lookup, kind-mismatch branch, unknown-token branch.
//!
//! ## KDL-level (T-041 R5)
//!
//! Pin the `validate_scene!` proc-macro emitting `.kdl:line:col`
//! pointers + plain-English diagnostics:
//!
//! - `undeclared_view_type.rs` — scene references a token no manifest
//!   declares.
//! - `view_type_mismatch_on_handle_attr.rs` — pane-kind context with a
//!   stack-declared view-type (or vice versa).
//! - `stack_child_under_non_stack_parent.rs` — `spawn_into @parent`
//!   where `@parent` resolves to a `Pane<V>`, not a `Stack<V>`.
//! - `handle_typed_attr_takes_non_handle.rs` — a manifest-declared
//!   `Pane<V>` attr receives a plain string literal instead of an
//!   `@handle` reference.
//!
//! The two compile-pass fixtures are ALSO extended to invoke
//! `validate_scene!` on the green path, so the macro's happy-path
//! expansion is pinned alongside the Rust surface.
//!
//! # Regenerating `.stderr` goldens
//!
//! Run this test with `TRYBUILD=overwrite` to bless new or updated
//! stderr output:
//!
//! ```sh
//! TRYBUILD=overwrite cargo test -p ark-scene --test view_types_trybuild
//! ```
//!
//! Every KDL-level stderr golden under `tests/ui/*.stderr` MUST
//! contain a `.kdl:<line>:<col>` pointer — assertable via:
//!
//! ```sh
//! rg -n "\.kdl:\d+:\d+" crates/scene/tests/ui/*.stderr
//! ```

#[test]
fn view_types_compile_fail() {
    let t = trybuild::TestCases::new();
    // Rust-level (pre-R5)
    // E0308: ViewDecl.name is String; passing an integer literal is
    // rejected at Rust-type level.
    t.compile_fail("tests/ui/view_decl_wrong_field_type.rs");
    // E0063: ExtensionMetadata struct-literal requires every field;
    // omitting `views` (and friends) is rejected at Rust-type level.
    t.compile_fail("tests/ui/metadata_missing_views_field.rs");
    // KDL-level (T-041 R5) — proc-macro `compile_error!` goldens.
    // Each .stderr must carry `.kdl:line:col` (grep-assertable).
    t.compile_fail("tests/ui/undeclared_view_type.rs");
    t.compile_fail("tests/ui/view_type_mismatch_on_handle_attr.rs");
    t.compile_fail("tests/ui/stack_child_under_non_stack_parent.rs");
    t.compile_fail("tests/ui/handle_typed_attr_takes_non_handle.rs");
    // T-042 R6: manifest is sole source of intent registration
    // (decision #2). A scene referencing an intent not declared by
    // any loaded manifest produces a compile error pointing at the
    // offending KDL line.
    t.compile_fail("tests/ui/undeclared_intent_reference.rs");
}

#[test]
fn view_types_compile_pass() {
    let t = trybuild::TestCases::new();
    // Mixed pane + stack ViewDecls through the table builder +
    // validate_view_reference runtime call — PLUS a KDL-level green-
    // path invocation of `validate_scene!` (T-041 R5).
    t.pass("tests/ui/valid_pane_and_stack_decls.rs");
    // Two extensions contributing views under namespaced tokens;
    // cross-ext lookup + kind-mismatch / unknown-token error branches —
    // PLUS a KDL-level cross-extension `validate_scene!` invocation.
    t.pass("tests/ui/cross_ext_view_reference.rs");
}
