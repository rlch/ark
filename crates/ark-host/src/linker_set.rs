//! Closed set of per-cap-profile `Linker<PluginCtx>` variants.
//!
//! T-PP-031 (Tier 3): one linker per subset of `ark:cap/*` interfaces;
//! all variants constructed at startup, keyed by `CapsKey` bitset.
//! Approach C (`define_unknown_imports_as_traps`) is forbidden in
//! production (see cluster 3 §3.3).
//!
//! Tier 0 scaffold — body empty.
