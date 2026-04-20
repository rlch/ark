//! `InstancePre<PluginCtx>` cache keyed by `(content-hash, CapsKey)`.
//!
//! T-PP-034 (Tier 3): pre-compiled instances shared across
//! re-instantiations of the same plugin under the same cap profile.
//! Keyed by `sha2` hash of the `.wasm` bytes + the cap bitset so a
//! post-launch grant change invalidates cleanly.
//!
//! Tier 0 scaffold — body empty.
