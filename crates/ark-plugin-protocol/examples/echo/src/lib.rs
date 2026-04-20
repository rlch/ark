// filled in T-PP-018
//
// Tier 0 scaffold — this file is intentionally empty. The full echo
// plugin (guest-world exports, `ark:host/log` + `ark:cap/fs-read`
// imports, `#[derive(Plugin)]` section emission) lands in T-PP-018
// after the WIT contract (T-PP-012) and derive macro (T-PP-019) are
// in place.
//
// This crate is NOT a workspace member, so it does not participate in
// `cargo check --workspace` / `cargo test --workspace`. Do not add it
// as a member until T-PP-018 when wasm32-wasip2 builds are ready.
