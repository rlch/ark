//! `ark-plugin-sdk` — the single proc-macro crate plugin authors depend
//! on.
//!
//! T-PP-002 (cavekit-plugin-protocol R3, R9): scaffold only. The real
//! `#[derive(Plugin)]` derive is implemented across Tier 2:
//!
//! - T-PP-019: caps half — parses `capabilities = [...]`, emits
//!   `#[link_section = "ark-caps:v1"]` postcard-encoded `CapsManifest`.
//! - T-PP-020: meta half — parses `name`/`version`/`abi` attributes,
//!   emits `#[link_section = "ark-meta:v1"]` postcard-encoded
//!   `MetaManifest`.
//! - T-PP-021: WIT world-name cross-check at compile time.
//!
//! Tier 0 ships an intentionally empty entrypoint so the crate
//! compiles (proc-macro crates must export at least one item of type
//! `fn(TokenStream) -> TokenStream` marked `#[proc_macro]` or
//! `#[proc_macro_derive]` — or nothing at all if the crate is
//! skeleton-only). We pick "nothing at all" here; the first real
//! derive lands in T-PP-019.

// TODO(T-PP-019): #[proc_macro_derive(Plugin, attributes(plugin))]
// pub fn derive_plugin(input: TokenStream) -> TokenStream { ... }
