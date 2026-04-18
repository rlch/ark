//! T-031 (claude-code-ext R5 handle-type validation) trybuild harness.
//!
//! Drives the KDL-level `validate_scene!` proc-macro (re-exported by
//! `ark-scene` from `ark-scene-macros`) against scene fixtures that
//! wire the `claude-code` view's `subagents` attribute to mismatched
//! handle kinds. Three fixtures:
//!
//! * `handle_kind_mismatch.rs` — scene declares a subagent view under
//!   a `pane` context, but the manifest declares kind `stack`. Expect
//!   compile error naming the kind mismatch (`.kdl:line:col` pointer).
//! * `unknown_subagents_view.rs` — scene references a view-type no
//!   manifest declares. Expect compile error naming the unknown token.
//! * `valid_subagents_decl.rs` — compile-pass. Same scene body as the
//!   production case with properly-declared views.
//!
//! # Approach
//!
//! Per the task brief, we use the KDL-level `validate_scene!` proc-
//! macro from `ark-scene-macros` (re-exported at `ark_scene::validate_scene`).
//! That macro directly supports the handle-kind validation semantics
//! T-031 requires — scene context (pane vs stack) must match the
//! manifest's declared `kind` for the referenced view-type. No Rust-
//! level type-system fixture is needed.
//!
//! # Regenerating `.stderr` goldens
//!
//! ```sh
//! TRYBUILD=overwrite cargo test -p ark-ext-claude-code --test handle_type_trybuild
//! ```
//!
//! Every `.stderr` under `tests/ui/` MUST contain a `.kdl:<line>:<col>`
//! pointer — assertable via:
//!
//! ```sh
//! rg -n "\.kdl:\d+:\d+" extensions/claude-code/tests/ui/*.stderr
//! ```

#[test]
fn handle_type_compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/handle_kind_mismatch.rs");
    t.compile_fail("tests/ui/unknown_subagents_view.rs");
}

#[test]
fn handle_type_compile_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/valid_subagents_decl.rs");
}
