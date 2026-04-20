//! Closed set of per-cap-profile `Linker<PluginCtx>` variants.
//!
//! T-PP-031 (Tier 3): one linker per subset of `ark:cap/*` interfaces;
//! all variants constructed at startup, keyed by `CapsKey` bitset.
//! Approach C (define-unknown-imports-as-traps) is forbidden in
//! production (see cluster 3 §3.3 and the forbidden-API lint in
//! `tests/lint_forbidden_apis.rs`).
//!
//! Tier 0 scaffold — body empty.
